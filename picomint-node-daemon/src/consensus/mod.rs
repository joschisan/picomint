pub mod api;
pub mod bft;
pub mod db;
pub mod ecash;
pub mod engine;
pub mod lightning;
pub mod onchain;
pub mod rpc;
pub mod server;
pub mod swap;
pub mod tx;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::ensure;
use bitcoin::Network;
use futures::TryFutureExt;
use picomint_bitcoind::BitcoindClient;
use picomint_core::methods::Method;
use picomint_core::tx::ConsensusItem;
use picomint_core::version::CONSENSUS_VERSION;
use picomint_core::wire;
use picomint_redb::{Database, DbRead};
use tokio::sync::watch;
use tokio::time::sleep;
use tracing::{info, warn};

use crate::config::{DaemonSettings, NodeConfig};
use crate::consensus::api::ConsensusApi;
use crate::consensus::db::{BlockHeightVoteTable, ConsensusVersionVoteTable};
use crate::consensus::server::Server;
use crate::p2p::{P2PStatusReceivers, ReconnectP2PConnections};

/// Confirmations a transaction needs before the mint treats it as final,
/// counted the usual way: the block that mines it is the first.
pub const CONFIRMATIONS: u32 = 6;

/// How many txs can be stored in memory before blocking the API.
///
/// What a submission costs us is its size, and a transaction is only bounded
/// by the inputs and outputs it may carry — so this is what turns that bound
/// into a bound on the memory a client can make us hold.
const TX_BUFFER: usize = 100;

pub async fn run(
    cfg: NodeConfig,
    settings: DaemonSettings,
    db: Database,
    btc_rpc: Arc<BitcoindClient>,
    connections: ReconnectP2PConnections,
    p2p_status_receivers: P2PStatusReceivers,
    foreign_conn_rx: async_channel::Receiver<iroh::endpoint::Connection>,
) -> anyhow::Result<()> {
    crate::config::validate_config(&cfg)?;

    anyhow::ensure!(
        cfg.consensus.network != bitcoin::Network::Bitcoin,
        "Picomint is experimental software and refuses to run a mint on mainnet"
    );

    let server = Server {
        cfg: cfg.clone(),
        db: db.clone(),
        btc_rpc: btc_rpc.clone(),
        rejected: watch::Sender::new(BTreeMap::new()),
        unordered: watch::Sender::new(false),
        integration_test: settings.integration_test,
    };

    onchain::spawn_broadcast_unconfirmed_txs_task(
        btc_rpc.clone(),
        db.clone(),
        settings.integration_test,
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

    let proposal_interval = if settings.integration_test {
        Duration::from_millis(100)
    } else {
        Duration::from_secs(5)
    };

    tokio::spawn(submit_ci_proposals(
        consensus_api.server.clone(),
        submission_tx.clone(),
        proposal_interval,
    ));

    let cli_router = crate::cli::router(consensus_api.clone());

    tokio::spawn(picomint_cli_server::serve(&settings.data_dir, cli_router)?);

    await_bitcoin_sync(&btc_rpc, cfg.consensus.network).await?;

    info!("Starting Consensus Engine...");

    engine::run(server, connections, submission_rx).await?;

    Ok(())
}

async fn await_bitcoin_sync(btc_rpc: &BitcoindClient, network: Network) -> anyhow::Result<()> {
    loop {
        match backend_sync(btc_rpc).await {
            Ok((backend_network, progress)) => {
                ensure!(
                    backend_network == network,
                    "Bitcoin backend network does not match",
                );

                if progress >= 0.999 {
                    return Ok(());
                }

                info!(
                    "Waiting for bitcoin backend to sync... {:.1}%",
                    progress * 100.0
                );
            }
            Err(error) => info!(%error, "Waiting to connect to bitcoin backend..."),
        }

        sleep(Duration::from_secs(1)).await;
    }
}

async fn backend_sync(btc_rpc: &BitcoindClient) -> anyhow::Result<(Network, f64)> {
    Ok((btc_rpc.network().await?, btc_rpc.get_sync_progress().await?))
}

async fn submit_ci_proposals(
    server: Server,
    submission_tx: async_channel::Sender<ConsensusItem>,
    interval: Duration,
) {
    let mut interval = tokio::time::interval(interval);

    loop {
        let dbtx = server.db.begin_read();

        if let Ok(block_height) = server
            .btc_rpc
            .get_block_height()
            .await
            .inspect_err(|error| warn!(%error, "Failed to fetch the block height to vote on"))
        {
            let current_vote = dbtx
                .get(&BlockHeightVoteTable, &server.cfg.private.identity)
                .unwrap_or(0);

            if block_height > current_vote {
                submission_tx
                    .send(ConsensusItem::BlockHeight(block_height))
                    .await
                    .ok();
            }
        }

        // Upgrading the binary is the whole of casting a vote: we
        // announce what we support until consensus has recorded it,
        // then stay quiet until the next upgrade raises it again. A
        // mint created by this binary has nothing to announce.
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

        for item in onchain::consensus_proposal(&server, &dbtx).await {
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
        Method::Ecash(m) => ecash::handle_api(&consensus_api.server, m).await,
        Method::Onchain(m) => onchain::handle_api(&consensus_api.server, m).await,
        Method::Lightning(m) => lightning::handle_api(&consensus_api.server, m).await,
        Method::Swap(m) => swap::handle_api(&consensus_api.server, m).await,
    }
}
