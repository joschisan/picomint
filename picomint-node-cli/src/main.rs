use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};

use anyhow::{Context as _, Result, ensure};
use clap::{Parser, Subcommand};
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper::body::Bytes;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use picomint_node_cli_core::{
    CLI_SOCKET_FILENAME, ExpirySetRequest, InviteRequest, LightningGatewayAddRequest,
    LightningGatewayRemoveRequest, ROUTE_AUDIT, ROUTE_BITCOIN_CONNECTION, ROUTE_BLOCK_COUNT,
    ROUTE_CONFIG, ROUTE_EXPIRY_CLEAR, ROUTE_EXPIRY_SET, ROUTE_EXPIRY_STATUS, ROUTE_INVITE,
    ROUTE_MODULE_LN_GATEWAY_ADD, ROUTE_MODULE_LN_GATEWAY_LIST, ROUTE_MODULE_LN_GATEWAY_REMOVE,
    ROUTE_MODULE_ONCHAIN_FEERATE, ROUTE_MODULE_ONCHAIN_PENDING_TXS,
    ROUTE_MODULE_ONCHAIN_TOTAL_VALUE, ROUTE_MODULE_ONCHAIN_TXS, ROUTE_P2P, ROUTE_SESSION_COUNT,
    ROUTE_SETUP_ADD_NODE, ROUTE_SETUP_RESTORE, ROUTE_SETUP_SET_LOCAL_PARAMS, ROUTE_SETUP_START_DKG,
    ROUTE_SETUP_STATUS, SetupAddNodeRequest, SetupSetLocalParamsRequest,
};
use serde::Serialize;
use serde_json::Value;
use tokio::net::UnixStream;
use tower_service::Service;

#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Path to the node's data directory (must match the daemon's
    /// `DATA_DIR`). The CLI finds the admin Unix socket at
    /// `{DATA_DIR}/cli.sock`.
    #[arg(long = "data-dir", env = "DATA_DIR")]
    data_dir: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Setup commands (DKG)
    #[command(subcommand)]
    Setup(SetupCommands),
    /// Generate a mint invite code
    Invite(InviteRequest),
    /// Show mint audit summary
    Audit,
    /// Dump full server config as JSON (use `> config.json` to save)
    Config,
    /// Number of consensus sessions this node has finalized
    SessionCount,
    /// Get the mint's consensus block count
    BlockCount,
    /// Per-node p2p connection status
    P2p,
    /// Status of the local bitcoin backend
    BitcoinConnection,
    /// Mint expiry announcement
    #[command(subcommand)]
    Expiry(ExpiryCommands),
    /// Module admin commands
    #[command(subcommand)]
    Module(ModuleCommands),
}

#[derive(Subcommand)]
enum ExpiryCommands {
    /// Announce a mint expiry
    Set(ExpirySetRequest),
    /// Clear the announced expiry
    Clear,
    /// Show the announced expiry (this node's local view)
    Status,
}

#[derive(Subcommand)]
enum SetupCommands {
    /// Check setup status
    Status,
    /// Set local node parameters
    SetLocalParams(SetupSetLocalParamsRequest),
    /// Add a node's setup code
    AddNode(SetupAddNodeRequest),
    /// Start distributed key generation
    StartDkg,
    /// Restore node config from a config file (skips DKG)
    Restore {
        /// Path to a `config.json` previously produced by `config`
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum ModuleCommands {
    /// Onchain module commands
    #[command(subcommand)]
    Onchain(OnchainCommands),
    /// Lightning module commands
    #[command(subcommand)]
    Lightning(LightningCommands),
}

#[derive(Subcommand)]
enum OnchainCommands {
    /// Get total onchain value
    TotalValue,
    /// Get consensus fee rate
    Feerate,
    /// Get pending transactions
    PendingTxs,
    /// Get transactions
    Txs,
}

#[derive(Subcommand)]
enum LightningCommands {
    /// Gateway management
    #[command(subcommand)]
    Gateway(LightningGatewayCommands),
}

#[derive(Subcommand)]
enum LightningGatewayCommands {
    /// Add a vetted gateway
    Add(LightningGatewayAddRequest),
    /// Remove a vetted gateway
    Remove(LightningGatewayRemoveRequest),
    /// List vetted gateways
    List,
}

/// Tiny connector that dials a fixed Unix socket path, ignoring the URI
/// entirely. Plugs into `hyper_util::client::legacy::Client` where a TCP
/// connector would normally go.
#[derive(Clone)]
struct UnixConnector {
    path: PathBuf,
}

impl Service<hyper::Uri> for UnixConnector {
    type Response = TokioIo<UnixStream>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<TokioIo<UnixStream>>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: hyper::Uri) -> Self::Future {
        let path = self.path.clone();
        Box::pin(async move { UnixStream::connect(path).await.map(TokioIo::new) })
    }
}

async fn request<R: Serialize>(data_dir: &Path, route: &str, payload: R) -> Result<Value> {
    let socket_path = data_dir.join(CLI_SOCKET_FILENAME);
    let connector = UnixConnector {
        path: socket_path.clone(),
    };
    let client = Client::builder(TokioExecutor::new()).build(connector);

    let body_bytes = serde_json::to_vec(&payload)?;
    let uri: hyper::Uri = format!("http://localhost{route}").parse()?;
    let req = Request::post(uri)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body_bytes)))?;

    let resp = client.request(req).await.with_context(|| {
        format!(
            "Failed to POST {route} to node at {}",
            socket_path.display()
        )
    })?;

    let status = resp.status();
    let resp_bytes = resp.into_body().collect().await?.to_bytes();

    ensure!(
        status.is_success(),
        "API error ({}): {}",
        status.as_u16(),
        String::from_utf8_lossy(&resp_bytes)
    );

    if resp_bytes.is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_slice(&resp_bytes).context("Failed to parse response")
    }
}

fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("Cannot serialize")
    );
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let d = &cli.data_dir;

    let result = match cli.command {
        Commands::Invite(req) => request(d, ROUTE_INVITE, req).await?,
        Commands::Audit => request(d, ROUTE_AUDIT, ()).await?,
        Commands::Config => request(d, ROUTE_CONFIG, ()).await?,
        Commands::SessionCount => request(d, ROUTE_SESSION_COUNT, ()).await?,
        Commands::BlockCount => request(d, ROUTE_BLOCK_COUNT, ()).await?,
        Commands::P2p => request(d, ROUTE_P2P, ()).await?,
        Commands::BitcoinConnection => request(d, ROUTE_BITCOIN_CONNECTION, ()).await?,

        Commands::Expiry(cmd) => match cmd {
            ExpiryCommands::Set(req) => request(d, ROUTE_EXPIRY_SET, req).await?,
            ExpiryCommands::Clear => request(d, ROUTE_EXPIRY_CLEAR, ()).await?,
            ExpiryCommands::Status => request(d, ROUTE_EXPIRY_STATUS, ()).await?,
        },

        Commands::Setup(cmd) => match cmd {
            SetupCommands::Status => request(d, ROUTE_SETUP_STATUS, ()).await?,
            SetupCommands::SetLocalParams(req) => {
                request(d, ROUTE_SETUP_SET_LOCAL_PARAMS, req).await?
            }
            SetupCommands::AddNode(req) => request(d, ROUTE_SETUP_ADD_NODE, req).await?,
            SetupCommands::StartDkg => request(d, ROUTE_SETUP_START_DKG, ()).await?,
            SetupCommands::Restore { path } => {
                let bytes = std::fs::read(&path)?;
                let cfg: Value = serde_json::from_slice(&bytes)?;
                request(d, ROUTE_SETUP_RESTORE, cfg).await?
            }
        },

        Commands::Module(cmd) => match cmd {
            ModuleCommands::Onchain(cmd) => match cmd {
                OnchainCommands::TotalValue => {
                    request(d, ROUTE_MODULE_ONCHAIN_TOTAL_VALUE, ()).await?
                }
                OnchainCommands::Feerate => request(d, ROUTE_MODULE_ONCHAIN_FEERATE, ()).await?,
                OnchainCommands::PendingTxs => {
                    request(d, ROUTE_MODULE_ONCHAIN_PENDING_TXS, ()).await?
                }
                OnchainCommands::Txs => request(d, ROUTE_MODULE_ONCHAIN_TXS, ()).await?,
            },
            ModuleCommands::Lightning(cmd) => match cmd {
                LightningCommands::Gateway(cmd) => match cmd {
                    LightningGatewayCommands::Add(req) => {
                        request(d, ROUTE_MODULE_LN_GATEWAY_ADD, req).await?
                    }
                    LightningGatewayCommands::Remove(req) => {
                        request(d, ROUTE_MODULE_LN_GATEWAY_REMOVE, req).await?
                    }
                    LightningGatewayCommands::List => {
                        request(d, ROUTE_MODULE_LN_GATEWAY_LIST, ()).await?
                    }
                },
            },
        },
    };

    print_json(&result);
    Ok(())
}
