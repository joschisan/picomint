pub mod api;
pub mod bft;
pub mod db;
pub mod engine;
pub mod lightning;
pub mod ecash;
pub mod rpc;
pub mod server;
pub mod tx;
pub mod onchain;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::bitcoind::{BitcoindClient, BitcoindRpcMonitor};
use anyhow::ensure;
use bitcoin::Network;
use futures::TryFutureExt;
use picomint_core::module::Method;
use picomint_core::tx::ConsensusItem;
use picomint_core::version::CONSENSUS_VERSION;
use picomint_core::wire;
use picomint_redb::{Database, DbRead};
use tokio::sync::watch;
use tokio::time::sleep;
use tracing::{info, warn};

use crate::config::{ConfigGenSettings, ServerConfig};
use crate::consensus::api::ConsensusApi;
use crate::consensus::db::{BlockCountVoteTable, ConsensusVersionVoteTable};
use crate::consensus::server::Server;
use crate::p2p::{P2PStatusReceivers, ReconnectP2PConnections};

/// Number of confirmations required for a transaction to be considered as
/// final by the federation. The block that mines the transaction does
/// not count towards the number of confirmations.
pub const CONFIRMATION_FINALITY_DELAY: u32 = 9;

/// How many txs can be stored in memory before blocking the API.
///
/// What a submission costs us is its size, and a transaction is only bounded
/// by the inputs and outputs it may carry — so this is what turns that bound
/// into a bound on the memory a client can make us hold.
const TX_BUFFER: usize = 100;

pub async fn run(
    cfg: ServerConfig,
    settings: ConfigGenSettings,
    db: Database,
    btc_rpc: Arc<BitcoindClient>,
    connections: ReconnectP2PConnections,
    p2p_status_receivers: P2PStatusReceivers,
    foreign_conn_rx: async_channel::Receiver<iroh::endpoint::Connection>,
) -> anyhow::Result<()> {
    cfg.validate_config()?;

    let btc_rpc = BitcoindRpcMonitor::new(
        btc_rpc,
        if cfg.consensus.network == Network::Regtest {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(10)
        },
    );

    let server = Server {
        cfg: cfg.clone(),
        db: db.clone(),
        btc_rpc: btc_rpc.clone(),
        rejected: watch::Sender::new(BTreeMap::new()),
    };

    onchain::spawn_broadcast_unconfirmed_txs_task(
        btc_rpc.clone(),
        db.clone(),
        cfg.consensus.network,
    );

    let (submission_tx, submission_rx) = async_channel::bounded(TX_BUFFER);

    let consensus_api = Arc::new(ConsensusApi {
        server: server.clone(),
        submission_tx: submission_tx.clone(),
        p2p_status_receivers,
    });

    info!("Starting Consensus Api...");

    tokio::spawn(run_iroh_api(consensus_api.clone(), foreign_conn_rx));

    info!("Starting Submission of Module CI proposals...");

    tokio::spawn(submit_ci_proposals(
        consensus_api.server.clone(),
        submission_tx.clone(),
    ));

    let ui_router = crate::ui::dashboard::router(consensus_api.clone());

    tokio::spawn(crate::ui::run(settings.ui_addr, ui_router));

    let cli_router = crate::cli::router(consensus_api.clone());

    tokio::spawn(crate::cli::run(settings.data_dir.clone(), cli_router));

    await_bitcoin_sync(&btc_rpc, cfg.consensus.network).await?;

    info!("Starting Consensus Engine...");

    engine::run(server, connections, submission_rx).await?;

    Ok(())
}

async fn await_bitcoin_sync(
    bitcoin_rpc_connection: &BitcoindRpcMonitor,
    network: Network,
) -> anyhow::Result<()> {
    loop {
        match bitcoin_rpc_connection.status() {
            Some(status) => {
                ensure!(
                    status.network == network,
                    "Bitcoin backend network does not match",
                );

                if let Some(progress) = status.sync_progress {
                    if progress >= 0.999 {
                        return Ok(());
                    }

                    info!(
                        "Waiting for bitcoin backend to sync... {:.1}%",
                        progress * 100.0
                    );
                } else {
                    return Ok(());
                }
            }
            None => {
                info!("Waiting to connect to bitcoin backend...");
            }
        }

        sleep(Duration::from_secs(1)).await;
    }
}

async fn submit_ci_proposals(server: Server, submission_tx: async_channel::Sender<ConsensusItem>) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));

    loop {
        let dbtx = server.db.begin_read();

        if let Some(status) = server.btc_rpc.status() {
            let block_count_vote = status
                .block_count
                .saturating_sub(CONFIRMATION_FINALITY_DELAY);

            let current_vote = dbtx
                .get(&BlockCountVoteTable, &server.cfg.private.identity)
                .unwrap_or(0);

            if block_count_vote > current_vote {
                submission_tx
                    .send(ConsensusItem::BlockCount(block_count_vote))
                    .await
                    .ok();
            }
        }

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

        for item in onchain::consensus_proposal(&server, &dbtx) {
            submission_tx
                .send(ConsensusItem::Module(wire::ModuleConsensusItem::Onchain(
                    item,
                )))
                .await
                .ok();
        }

        interval.tick().await;
    }
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
        Method::Core(m) => rpc::handle_api(&consensus_api, m).await,
        Method::ECash(m) => ecash::handle_api(&consensus_api.server, m).await,
        Method::Onchain(m) => onchain::handle_api(&consensus_api.server, m).await,
        Method::Lightning(m) => lightning::handle_api(&consensus_api.server, m).await,
    }
}
