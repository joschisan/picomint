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

use std::sync::Arc;
use std::time::Duration;

use bitcoin::Network;
use futures::TryFutureExt;
use picomint_bitcoin_rpc::{BitcoinRpcMonitor, BitcoindClient};
use picomint_core::module::Method;
use picomint_core::tx::ConsensusItem;
use picomint_core::version::CONSENSUS_VERSION;
use picomint_core::wire;
use picomint_sqlite::{Database, DbRead};
use tokio::net::TcpListener;
use tokio::time::sleep;
use tracing::{info, warn};

use crate::config::{ConfigGenSettings, ServerConfig};
use crate::consensus::api::ConsensusApi;
use crate::consensus::db::ConsensusVersionVoteTable;
use crate::consensus::server::Server;
use crate::p2p::{P2PStatusReceivers, ReconnectP2PConnections};

/// How many txs can be stored in memory before blocking the API.
///
/// What a submission costs us is its size, and a transaction is only bounded
/// by the inputs and outputs it may carry — so this is what turns that bound
/// into a bound on the memory a client can make us hold.
const TX_BUFFER: usize = 100;

/// How many rejected txs a waiting submission RPC can fall behind before it
/// misses one. Only finally rejected txs are broadcast, so the steady-state
/// rate is zero and the buffer only has to absorb bursts of invalid txs.
const TX_REJECT_BUFFER: usize = 1000;

pub async fn run(
    cfg: ServerConfig,
    settings: ConfigGenSettings,
    db: Database,
    bitcoin_backend: Arc<BitcoindClient>,
    connections: ReconnectP2PConnections,
    p2p_status_receivers: P2PStatusReceivers,
    foreign_conn_rx: async_channel::Receiver<iroh::endpoint::Connection>,
) -> anyhow::Result<()> {
    cfg.validate_config()?;

    let bitcoin_rpc_connection = BitcoinRpcMonitor::new(
        bitcoin_backend,
        if cfg.consensus.network == Network::Regtest {
            Duration::from_millis(100)
        } else {
            Duration::from_mins(1)
        },
    );

    let server = Server {
        cfg: cfg.clone(),
        db: db.clone(),
        btc_rpc: bitcoin_rpc_connection.clone(),
    };

    wallet::spawn_broadcast_unconfirmed_txs_task(
        bitcoin_rpc_connection.clone(),
        db.clone(),
        cfg.consensus.network,
    );

    let (submission_tx, submission_rx) = async_channel::bounded(TX_BUFFER);

    let (tx_reject_tx, _) = tokio::sync::broadcast::channel(TX_REJECT_BUFFER);

    let consensus_api = Arc::new(ConsensusApi {
        server: server.clone(),
        submission_tx: submission_tx.clone(),
        tx_reject_tx: tx_reject_tx.clone(),
        p2p_status_receivers,
    });

    info!("Starting Consensus Api...");

    tokio::spawn(run_iroh_api(consensus_api.clone(), foreign_conn_rx));

    info!("Starting Submission of Module CI proposals...");

    tokio::spawn({
        let server = consensus_api.server.clone();
        let submission_tx = submission_tx.clone();
        async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                let dbtx = server.db.begin_read();
                // Upgrading the binary is the whole of casting a vote: we
                // announce what we support until consensus has recorded it,
                // then stay quiet until the next upgrade raises it again. A
                // federation created by this binary has nothing to announce.
                if dbtx
                    .get(&ConsensusVersionVoteTable, &server.cfg.private.identity)
                    .unwrap_or(server.cfg.consensus.default_version)
                    < CONSENSUS_VERSION
                {
                    submission_tx
                        .send(ConsensusItem::Version(CONSENSUS_VERSION))
                        .await
                        .ok();
                }
                for item in wallet::consensus_proposal(&server, &dbtx) {
                    submission_tx
                        .send(ConsensusItem::Module(wire::ModuleConsensusItem::Wallet(
                            item,
                        )))
                        .await
                        .ok();
                }
                for item in ln::consensus_proposal(&server, &dbtx) {
                    submission_tx
                        .send(ConsensusItem::Module(wire::ModuleConsensusItem::Ln(item)))
                        .await
                        .ok();
                }
                interval.tick().await;
            }
        }
    });

    let ui_service = crate::ui::dashboard::router(consensus_api.clone()).into_make_service();

    let ui_listener = TcpListener::bind(settings.ui_addr)
        .await
        .expect("Failed to bind dashboard UI");

    tokio::spawn(async move {
        axum::serve(ui_listener, ui_service)
            .await
            .expect("Failed to serve dashboard UI");
    });

    info!("Dashboard UI running at http://{} 🚀", settings.ui_addr);

    {
        let data_dir = settings.data_dir.clone();
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

                    info!(
                        "Waiting for bitcoin backend to sync... {:.1}%",
                        progress * 100.0
                    );
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

    engine::run(server, connections, submission_rx, tx_reject_tx).await?;

    Ok(())
}

async fn run_iroh_api(
    consensus_api: Arc<ConsensusApi>,
    foreign_conn_rx: async_channel::Receiver<iroh::endpoint::Connection>,
) {
    while let Ok(connection) = foreign_conn_rx.recv().await {
        let consensus_api = consensus_api.clone();
        tokio::spawn(
            picomint_rpc::handle_request(connection, move |method| {
                dispatch(consensus_api.clone(), method)
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
        Method::Mint(m) => mint::handle_api(&consensus_api.server, m).await,
        Method::Wallet(m) => wallet::handle_api(&consensus_api.server, m).await,
        Method::Ln(m) => ln::handle_api(&consensus_api.server, m).await,
    }
}
