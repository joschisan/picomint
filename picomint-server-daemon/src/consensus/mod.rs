pub mod api;
pub mod bft;
pub mod db;
pub mod engine;
pub mod ln;
pub mod mint;
mod rpc;
pub mod server;
pub mod transaction;
pub mod wallet;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bitcoin::Network;
use futures::FutureExt;
use iroh::endpoint::{RecvStream, SendStream};
use picomint_bitcoin_rpc::{BitcoinBackend, BitcoinRpcMonitor};
use picomint_core::NumPeers;
use picomint_core::module::{ApiAuth, ApiError, Method};
use picomint_core::transaction::ConsensusItem;
use picomint_core::wire;
use picomint_encoding::{Decodable, Encodable};
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

/// Maximum number of concurrent iroh connections on the public API.
const MAX_CONNECTIONS: usize = 1000;

/// Maximum number of parallel requests per iroh API connection.
const MAX_REQUESTS_PER_CONNECTION: usize = 50;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    connections: ReconnectP2PConnections<P2PMessage>,
    p2p_status_receivers: P2PStatusReceivers,
    foreign_conn_rx: async_channel::Receiver<iroh::endpoint::Connection>,
    cfg: ServerConfig,
    db: Database,
    bitcoin_backend: Arc<BitcoinBackend>,
    ui_config: Option<(SocketAddr, ApiAuth)>,
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

    let mut ci_status_senders = BTreeMap::new();
    let mut ci_status_receivers = BTreeMap::new();

    for peer in cfg.consensus.peers.keys().copied() {
        let (ci_tx, ci_rx) = watch::channel(None);

        ci_status_senders.insert(peer, ci_tx);
        ci_status_receivers.insert(peer, ci_rx);
    }

    let consensus_api = Arc::new(ConsensusApi {
        cfg: cfg.clone(),
        db: db.clone(),
        server: server.clone(),
        submission_tx: submission_tx.clone(),
        shutdown_tx,
        shutdown_rx: shutdown_rx.clone(),
        p2p_status_receivers,
        ci_status_receivers,
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
                for item in server.mint.consensus_proposal(&dbtx.as_ref()).await {
                    submission_tx
                        .send(ConsensusItem::Module(wire::ModuleConsensusItem::Mint(item)))
                        .await
                        .ok();
                }
                for item in server.wallet.consensus_proposal(&dbtx.as_ref()).await {
                    submission_tx
                        .send(ConsensusItem::Module(wire::ModuleConsensusItem::Wallet(
                            item,
                        )))
                        .await
                        .ok();
                }
                for item in server.ln.consensus_proposal(&dbtx.as_ref()).await {
                    submission_tx
                        .send(ConsensusItem::Module(wire::ModuleConsensusItem::Ln(item)))
                        .await
                        .ok();
                }
                interval.tick().await;
            }
        }
    });

    if let Some((ui_addr, auth)) = ui_config {
        let ui_service =
            crate::ui::dashboard::router(consensus_api.clone(), auth).into_make_service();
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
        ci_status_senders,
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
    let parallel_connections_limit = Arc::new(Semaphore::new(MAX_CONNECTIONS));

    while let Ok(connection) = foreign_conn_rx.recv().await {
        if parallel_connections_limit.available_permits() == 0 {
            warn!(
                limit = MAX_CONNECTIONS,
                "Iroh API connection limit reached, blocking new connections"
            );
        }
        let permit = parallel_connections_limit
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore should not be closed");
        tokio::spawn(
            handle_incoming(consensus_api.clone(), connection, permit).then(|result| async {
                if let Err(err) = result {
                    warn!(err = %format_args!("{err:#}"), "Failed to handle iroh connection");
                }
            }),
        );
    }
}

async fn handle_incoming(
    consensus_api: Arc<ConsensusApi>,
    connection: iroh::endpoint::Connection,
    _connection_permit: tokio::sync::OwnedSemaphorePermit,
) -> anyhow::Result<()> {
    let parallel_requests_limit = Arc::new(Semaphore::new(MAX_REQUESTS_PER_CONNECTION));

    loop {
        let (send_stream, recv_stream) = connection.accept_bi().await?;

        if parallel_requests_limit.available_permits() == 0 {
            warn!(
                limit = MAX_REQUESTS_PER_CONNECTION,
                "Iroh API request limit reached for connection, blocking new requests"
            );
        }
        let permit = parallel_requests_limit
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore should not be closed");
        tokio::spawn(
            handle_request(consensus_api.clone(), send_stream, recv_stream, permit).then(
                |result| async {
                    if let Err(err) = result {
                        warn!(err = %format_args!("{err:#}"), "Failed to handle iroh request");
                    }
                },
            ),
        );
    }
}

async fn handle_request(
    consensus_api: Arc<ConsensusApi>,
    mut send_stream: SendStream,
    mut recv_stream: RecvStream,
    _request_permit: tokio::sync::OwnedSemaphorePermit,
) -> anyhow::Result<()> {
    let request = recv_stream.read_to_end(100_000).await?;
    let method = Method::consensus_decode(&request)?;

    let response = dispatch(consensus_api, method).await;
    let response = response.consensus_encode_to_vec();

    send_stream.write_all(&response).await?;
    send_stream.finish()?;
    Ok(())
}

async fn dispatch(consensus_api: Arc<ConsensusApi>, method: Method) -> Result<Vec<u8>, ApiError> {
    match method {
        Method::Core(m) => consensus_api.handle_api(m).await,
        Method::Mint(m) => consensus_api.server.mint.handle_api(m).await,
        Method::Wallet(m) => consensus_api.server.wallet.handle_api(m).await,
        Method::Ln(m) => consensus_api.server.ln.handle_api(m).await,
    }
}
