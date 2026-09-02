//! Federation guardian daemon.
//!
//! This crate hosts both the daemon library and the `picomint-guardian-daemon`
//! binary (`src/main.rs`). It drives config generation, consensus, and the
//! admin UI/CLI for the fixed module set (mint + lightning + wallet).

extern crate picomint_core;

pub mod cli;
pub mod config;
pub mod consensus;
pub mod p2p;
pub mod ui;

use std::sync::Arc;

/// Name of the server daemon's database file on disk.
pub const DB_FILE: &str = "database.redb";

use config::ServerConfig;
use picomint_bitcoin_rpc::BitcoindClient;
use picomint_redb::{Database, DbRead};
use tracing::info;

/// Dispatch helper for module `handle_api` match arms.
///
/// `handler!(fn_name, server, req).await` calls `rpc::fn_name(server, req)`
/// and consensus-encodes the response. Each module has a `mod rpc` submodule
/// with one `fn name(server: &Server, req: XRequest) -> Result<XResponse,
/// String>` per endpoint (the core API passes `&ConsensusApi` instead). Use
/// [`handler_async!`] when the rpc handler is itself async.
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

use crate::config::db::{ConfigGenParamsTable, load_server_config, store_server_config};
use crate::config::setup::SetupApi;
use crate::config::{ConfigGenParams, ConfigGenSettings, SetupResult};
use crate::p2p::{P2PConnector, ReconnectP2PConnections, p2p_status_channels};

pub async fn run_server(
    settings: ConfigGenSettings,
    db: Database,
    bitcoin: Arc<BitcoindClient>,
) -> anyhow::Result<()> {
    if let Some(cfg) = load_server_config(&db).await {
        return run_consensus(cfg, settings, db, bitcoin).await;
    }

    if let Some(cgp) = db.begin_read().get(&ConfigGenParamsTable, &()) {
        return run_dkg_then_consensus(cgp, settings, db, bitcoin).await;
    }

    info!("Starting setup UI...");

    let (setup_tx, mut setup_rx) = tokio::sync::mpsc::channel(1);

    let setup_api = Arc::new(SetupApi::new(settings.clone(), setup_tx, db.clone()));

    let setup_ui_handle = tokio::spawn(ui::run(
        settings.ui_addr,
        ui::setup::router(setup_api.clone()),
    ));

    info!("Setup UI running at http://{} 🚀", settings.ui_addr);

    let setup_cli_handle = tokio::spawn(cli::run_cli(settings.data_dir.clone(), setup_api.clone()));

    let setup_result = setup_rx
        .recv()
        .await
        .expect("Setup result receiver closed unexpectedly");

    // Tear down the setup UI/CLI listeners before the DKG phase rebinds
    // the same TCP port and Unix socket. Aborting drops the listener at
    // the next await; awaiting the handle confirms the bind is released.
    setup_ui_handle.abort();

    setup_ui_handle.await.ok();

    setup_cli_handle.abort();

    setup_cli_handle.await.ok();

    match setup_result {
        SetupResult::Dkg(cgp) => run_dkg_then_consensus(*cgp, settings, db, bitcoin).await,
        SetupResult::Restored(cfg) => run_consensus(*cfg, settings, db, bitcoin).await,
    }
}

async fn run_dkg_then_consensus(
    cgp: ConfigGenParams,
    settings: ConfigGenSettings,
    db: Database,
    bitcoin_rpc: Arc<BitcoindClient>,
) -> anyhow::Result<()> {
    info!("Starting DKG...");

    // Single channel for foreign (non-peer) iroh connections — fed by the
    // p2p accept loop's demux, drained by the consensus-phase api task.
    // Small bound: pre-DKG there's no consumer, so incoming api attempts
    // overflow and are dropped (no valid client should be talking to a
    // not-yet-bootstrapped federation).
    let (conn_tx, conn_rx) = async_channel::bounded(128);

    let cnt = P2PConnector::new(cgp.iroh_sk.clone(), cgp.iroh_pks(), settings.p2p_addr).await?;

    let (status_txs, status_rxs) = p2p_status_channels(cnt.peers());

    let connections = ReconnectP2PConnections::new(cgp.identity, cnt, status_txs, conn_tx);

    // Serve a stateless loading page on UI_ADDR while DKG runs.
    // Operators reloading the page during DKG — or opening it
    // for the first time after an auto-resume restart — get a
    // coherent waiting screen instead of a connection error.
    let dkg_ui_handle = tokio::spawn(ui::run(settings.ui_addr, ui::dkg::router(db.clone())));

    let cfg = ServerConfig::generate(&cgp, connections.clone(), status_rxs.clone()).await?;

    store_server_config(&db, &cfg).await;

    dkg_ui_handle.abort();

    dkg_ui_handle.await.ok();

    info!("Starting consensus...");

    consensus::run(
        cfg,
        settings,
        db,
        bitcoin_rpc,
        connections,
        status_rxs,
        conn_rx,
    )
    .await
}

async fn run_consensus(
    cfg: ServerConfig,
    settings: ConfigGenSettings,
    db: Database,
    bitcoin_rpc: Arc<BitcoindClient>,
) -> anyhow::Result<()> {
    // Single channel for foreign (non-peer) iroh connections — fed by the
    // p2p accept loop's demux, drained by the consensus-phase api task.
    // Small bound: pre-DKG there's no consumer, so incoming api attempts
    // overflow and are dropped (no valid client should be talking to a
    // not-yet-bootstrapped federation).
    let (conn_tx, conn_rx) = async_channel::bounded(128);

    let cnt = P2PConnector::new(
        cfg.private.iroh_sk.clone(),
        cfg.consensus
            .peers
            .iter()
            .map(|(peer, endpoint)| (*peer, endpoint.iroh_pk))
            .collect(),
        settings.p2p_addr,
    )
    .await?;

    let (status_txs, status_rxs) = p2p_status_channels(cnt.peers());

    let connections = ReconnectP2PConnections::new(cfg.private.identity, cnt, status_txs, conn_tx);

    info!("Starting consensus...");

    consensus::run(
        cfg,
        settings,
        db,
        bitcoin_rpc,
        connections,
        status_rxs,
        conn_rx,
    )
    .await?;

    Ok(())
}
