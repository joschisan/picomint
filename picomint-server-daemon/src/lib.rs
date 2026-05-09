//! Federation server daemon.
//!
//! This crate hosts both the daemon library and the `picomint-server-daemon`
//! binary (`src/main.rs`). It drives config generation, consensus, and the
//! admin UI/CLI for the fixed module set (mint + lightning + wallet).

extern crate picomint_core;

pub mod cli;
pub mod config;
pub mod consensus;
pub mod p2p;
pub mod ui;

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Name of the server daemon's database file on disk.
pub const DB_FILE: &str = "database.redb";

use config::ServerConfig;
use picomint_bitcoin_rpc::BitcoinBackend;
use picomint_redb::Database;
use tokio::net::TcpListener;
use tracing::info;

/// Dispatch helper for module `handle_api` match arms.
///
/// `handler!(fn_name, self, req).await` calls `rpc::fn_name(self, req)` and
/// consensus-encodes the response. Each module has a `mod rpc` submodule with
/// one `fn name(module: &Self, req: XRequest) -> Result<XResponse, ApiError>`
/// per endpoint. Use [`handler_async!`] when the rpc handler is itself async.
#[macro_export]
macro_rules! handler {
    ($func:ident, $self:expr, $req:expr) => {
        async move {
            let resp = rpc::$func($self, $req)?;
            ::std::result::Result::Ok(::picomint_encoding::Encodable::consensus_encode_to_vec(
                &resp,
            ))
        }
    };
}

/// Like [`handler!`] but for `async fn` rpc handlers.
#[macro_export]
macro_rules! handler_async {
    ($func:ident, $self:expr, $req:expr) => {
        async move {
            let resp = rpc::$func($self, $req).await?;
            ::std::result::Result::Ok(::picomint_encoding::Encodable::consensus_encode_to_vec(
                &resp,
            ))
        }
    };
}

use crate::config::db::{load_server_config, store_server_config};
use crate::config::setup::SetupApi;
use crate::config::{ConfigGenSettings, SetupResult};
use crate::p2p::{
    P2PConnector, P2PMessage, P2PStatusReceivers, ReconnectP2PConnections, p2p_status_channels,
};

pub async fn run_server(
    settings: ConfigGenSettings,
    db: Database,
    bitcoin_rpc: Arc<BitcoinBackend>,
    data_dir: PathBuf,
) -> anyhow::Result<()> {
    // Single channel for foreign (non-peer) iroh connections — fed by the
    // p2p accept loop's demux, drained by the consensus-phase api task.
    // Small bound: pre-DKG there's no consumer, so incoming api attempts
    // overflow and are dropped (no valid client should be talking to a
    // not-yet-bootstrapped federation).
    let (foreign_conn_tx, foreign_conn_rx) = async_channel::bounded(128);

    let (cfg, connections, p2p_status_receivers) = match load_server_config(&db).await {
        Some(cfg) => {
            let connector = P2PConnector::new(
                cfg.private.iroh_sk.clone(),
                settings.p2p_addr,
                cfg.consensus
                    .peers
                    .iter()
                    .map(|(peer, endpoint)| (*peer, endpoint.iroh_pk))
                    .collect(),
            )
            .await?;

            let (p2p_status_senders, p2p_status_receivers) = p2p_status_channels(connector.peers());

            let connections = ReconnectP2PConnections::<P2PMessage>::new(
                cfg.private.identity,
                connector,
                p2p_status_senders,
                foreign_conn_tx,
            );

            (cfg, connections, p2p_status_receivers)
        }
        None => {
            Box::pin(run_config_gen(
                db.clone(),
                settings.clone(),
                &data_dir,
                foreign_conn_tx,
            ))
            .await?
        }
    };

    info!("Starting consensus...");

    Box::pin(consensus::run(
        connections,
        p2p_status_receivers,
        foreign_conn_rx,
        cfg,
        db,
        bitcoin_rpc,
        settings.ui_config,
        &data_dir,
    ))
    .await?;

    Ok(())
}

pub async fn run_config_gen(
    db: Database,
    settings: ConfigGenSettings,
    data_dir: &Path,
    foreign_conn_tx: async_channel::Sender<iroh::endpoint::Connection>,
) -> anyhow::Result<(
    ServerConfig,
    ReconnectP2PConnections<P2PMessage>,
    P2PStatusReceivers,
)> {
    info!("Starting config gen");

    let (setup_tx, mut setup_rx) = tokio::sync::mpsc::channel(1);

    let setup_api = Arc::new(SetupApi::new(settings.clone(), setup_tx));

    let setup_ui_handle = if let Some((ui_addr, auth)) = settings.ui_config.clone() {
        let ui_service = ui::setup::router(setup_api.clone(), auth).into_make_service();
        let ui_listener = TcpListener::bind(ui_addr)
            .await
            .expect("Failed to bind setup UI");
        let handle = tokio::spawn(async move {
            axum::serve(ui_listener, ui_service)
                .await
                .expect("Failed to serve setup UI");
        });
        info!("Setup UI running at http://{ui_addr} 🚀");
        Some(handle)
    } else {
        info!("UI disabled (UI_ADDR unset); driving setup via CLI only");
        None
    };

    let cli_state = cli::CliState {
        setup_api: setup_api.clone(),
    };
    let cli_data_dir = data_dir.to_owned();
    let setup_cli_handle = tokio::spawn(async move {
        cli::run_cli(&cli_data_dir, cli_state).await;
    });

    let setup_result = setup_rx
        .recv()
        .await
        .expect("Setup result receiver closed unexpectedly");

    // Stop the setup UI/CLI listeners before consensus tries to bind the
    // same TCP port and Unix socket. Aborting drops the listener at the
    // next await; awaiting the handle confirms the bind is released.
    if let Some(handle) = setup_ui_handle {
        handle.abort();
        handle.await.ok();
    }
    setup_cli_handle.abort();
    setup_cli_handle.await.ok();

    match setup_result {
        SetupResult::Dkg(cg_params) => {
            let connector = P2PConnector::new(
                cg_params.iroh_sk.clone(),
                settings.p2p_addr,
                cg_params.iroh_pks(),
            )
            .await?;

            let (p2p_status_senders, p2p_status_receivers) = p2p_status_channels(connector.peers());

            let connections = ReconnectP2PConnections::<P2PMessage>::new(
                cg_params.identity,
                connector,
                p2p_status_senders,
                foreign_conn_tx,
            );

            let cfg = ServerConfig::distributed_gen(
                &cg_params,
                connections.clone(),
                p2p_status_receivers.clone(),
            )
            .await?;

            store_server_config(&db, &cfg).await;

            Ok((cfg, connections, p2p_status_receivers))
        }
        SetupResult::Recovered(cfg) => {
            let connector = P2PConnector::new(
                cfg.private.iroh_sk.clone(),
                settings.p2p_addr,
                cfg.consensus
                    .peers
                    .iter()
                    .map(|(peer, endpoint)| (*peer, endpoint.iroh_pk))
                    .collect(),
            )
            .await?;

            let (p2p_status_senders, p2p_status_receivers) = p2p_status_channels(connector.peers());

            let connections = ReconnectP2PConnections::<P2PMessage>::new(
                cfg.private.identity,
                connector,
                p2p_status_senders,
                foreign_conn_tx,
            );

            store_server_config(&db, &cfg).await;

            Ok((*cfg, connections, p2p_status_receivers))
        }
    }
}
