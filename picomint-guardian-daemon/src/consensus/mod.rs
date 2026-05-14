pub mod api;
pub mod bft;
pub mod db;
pub mod engine;
pub mod ln;
pub mod mint;
mod rpc;
pub mod server;
pub mod tx;
pub mod wallet;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bitcoin::Network;
use futures::TryFutureExt;
use picomint_bitcoin_rpc::{BitcoinBackend, BitcoinRpcMonitor};
use picomint_core::NumPeers;
use picomint_core::module::Method;
use picomint_core::tx::ConsensusItem;
use picomint_core::wire;
use picomint_redb::Database;
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};
use tokio::time::sleep;
use tracing::{info, warn};

use crate::config::ServerConfig;
use crate::consensus::api::ConsensusApi;
use crate::consensus::engine::ConsensusEngine;
use crate::consensus::server::Server;
use crate::p2p::{P2PMessage, P2PStatusReceivers, ReconnectP2PConnections};

/// How many txs can be stored in memory before blocking the API
const TX_BUFFER: usize = 1000;

/// Maximum number of concurrent in-flight iroh API requests, summed
/// across every accepted connection.
const MAX_CONCURRENT_REQUESTS: usize = 1000;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    connections: ReconnectP2PConnections<P2PMessage>,
    p2p_status_receivers: P2PStatusReceivers,
    foreign_conn_rx: async_channel::Receiver<iroh::endpoint::Connection>,
    cfg: ServerConfig,
    db: Database,
    bitcoin_backend: Arc<BitcoinBackend>,
    ui_addr: Option<SocketAddr>,
    data_dir: &Path,
) -> anyhow::Result<()> {
    cfg.validate_config(&cfg.private.identity)?;

    let bitcoin_rpc_connection = BitcoinRpcMonitor::new(
        bitcoin_backend,
        if cfg.consensus.network == Network::Regtest {
            Duration::from_millis(100)
        } else {
            Duration::from_mins(1)
        },
    );

    // Wait for the bitcoin backend to come up before instantiating modules that
    // read its status during startup (the wallet module broadcast loop).
    let _num_peers = NumPeers::from(cfg.consensus.peers.len());

    info!("Initialise module mint...");
    let mint = Arc::new(crate::consensus::mint::Mint::new(
        cfg.mint_config(),
        db.clone(),
    ));

    info!("Initialise module wallet...");
    let wallet = Arc::new(crate::consensus::wallet::Wallet::new(
        cfg.wallet_config(),
        db.clone(),
        bitcoin_rpc_connection.clone(),
        cfg.consensus.network,
    ));

    info!("Initialise module ln...");
    let ln = Arc::new(crate::consensus::ln::Lightning::new(
        cfg.ln_config(),
        db.clone(),
        bitcoin_rpc_connection.clone(),
    ));

    let server = Server { mint, wallet, ln };

    let (submission_tx, submission_rx) = async_channel::bounded(TX_BUFFER);
    let (shutdown_tx, shutdown_rx) = watch::channel(None);

    let consensus_api = Arc::new(ConsensusApi {
        cfg: cfg.clone(),
        db: db.clone(),
        server: server.clone(),
        submission_tx: submission_tx.clone(),
        shutdown_tx,
        shutdown_rx: shutdown_rx.clone(),
        p2p_status_receivers,
        bitcoin_rpc_connection: bitcoin_rpc_connection.clone(),
    });

    info!("Starting Consensus Api...");

    tokio::spawn(run_iroh_api(consensus_api.clone(), foreign_conn_rx));

    info!("Starting Submission of Module CI proposals...");

    tokio::spawn({
        let server = consensus_api.server.clone();
        let db = db.clone();
        let submission_tx = submission_tx.clone();
        async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                let dbtx = db.begin_read();
                for item in server.mint.consensus_proposal(&dbtx).await {
                    submission_tx
                        .send(ConsensusItem::Module(wire::ModuleConsensusItem::Mint(item)))
                        .await
                        .ok();
                }
                for item in server.wallet.consensus_proposal(&dbtx).await {
                    submission_tx
                        .send(ConsensusItem::Module(wire::ModuleConsensusItem::Wallet(
                            item,
                        )))
                        .await
                        .ok();
                }
                for item in server.ln.consensus_proposal(&dbtx).await {
                    submission_tx
                        .send(ConsensusItem::Module(wire::ModuleConsensusItem::Ln(item)))
                        .await
                        .ok();
                }
                interval.tick().await;
            }
        }
    });

    if let Some(ui_addr) = ui_addr {
        let ui_service = crate::ui::dashboard::router(consensus_api.clone()).into_make_service();
        let ui_listener = TcpListener::bind(ui_addr)
            .await
            .expect("Failed to bind dashboard UI");
        tokio::spawn(async move {
            axum::serve(ui_listener, ui_service)
                .await
                .expect("Failed to serve dashboard UI");
        });
        info!("Dashboard UI running at http://{ui_addr} 🚀");
    } else {
        info!("UI disabled (UI_ADDR unset); dashboard available via CLI only");
    }

    {
        let data_dir = data_dir.to_owned();
        let dashboard_router = crate::cli::dashboard_cli_router(consensus_api.clone());
        tokio::spawn(async move {
            crate::cli::run_dashboard_cli(&data_dir, dashboard_router).await;
        });
    }

    loop {
        match bitcoin_rpc_connection.status() {
            Some(status) => {
                anyhow::ensure!(
                    status.network == cfg.consensus.network,
                    "Bitcoin backend network {} does not match federation network {}",
                    status.network,
                    cfg.consensus.network,
                );

                if let Some(progress) = status.sync_progress {
                    if progress >= 0.999 {
                        break;
                    }

                    info!("Waiting for bitcoin backend to sync... {progress:.1}%");
                } else {
                    break;
                }
            }
            None => {
                info!("Waiting to connect to bitcoin backend...");
            }
        }

        sleep(Duration::from_secs(1)).await;
    }

    info!("Starting Consensus Engine...");

    ConsensusEngine {
        db,
        cfg: cfg.clone(),
        connections,
        submission_rx,
        shutdown_rx,
        server: consensus_api.server.clone(),
    }
    .run()
    .await?;

    Ok(())
}

async fn run_iroh_api(
    consensus_api: Arc<ConsensusApi>,
    foreign_conn_rx: async_channel::Receiver<iroh::endpoint::Connection>,
) {
    let request_limit = Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS));

    while let Ok(connection) = foreign_conn_rx.recv().await {
        let consensus_api = consensus_api.clone();
        tokio::spawn(
            picomint_rpc::handle_request(connection, request_limit.clone(), |method| {
                dispatch(consensus_api, method)
            })
            .inspect_err(|e| {
                warn!(?e, "Failed to handle iroh request");
            }),
        );
    }
}

async fn dispatch(consensus_api: Arc<ConsensusApi>, method: Method) -> Result<Vec<u8>, String> {
    match method {
        Method::Core(m) => consensus_api.handle_api(m).await,
        Method::Mint(m) => consensus_api.server.mint.handle_api(m).await,
        Method::Wallet(m) => consensus_api.server.wallet.handle_api(m).await,
        Method::Ln(m) => consensus_api.server.ln.handle_api(m).await,
    }
}
