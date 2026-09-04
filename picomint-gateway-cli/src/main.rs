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
use picomint_gateway_cli_core::{
    CLI_SOCKET_FILENAME, LdkChannelCloseRequest, LdkChannelOpenRequest, LdkChannelSpliceInRequest,
    LdkChannelSpliceOutRequest, LdkLightningProbeRequest, LdkLightningReceiveRequest,
    LdkLightningSendRequest, LdkOnchainSendRequest, LdkPeerConnectRequest,
    LdkPeerDisconnectRequest, MintAddRequest, MintBalanceRequest, MintConfigRequest,
    MintEcashCountRequest, MintEcashReceiveRequest, MintEcashSendRequest,
    MintOnchainReceiveRequest, MintOnchainSendFeeRequest, MintOnchainSendRequest,
    MintRemoveRequest, QueryRequest, ROUTE_INFO, ROUTE_LDK_BALANCES, ROUTE_LDK_CHANNEL_CLOSE,
    ROUTE_LDK_CHANNEL_LIST, ROUTE_LDK_CHANNEL_OPEN, ROUTE_LDK_CHANNEL_SPLICE_IN,
    ROUTE_LDK_CHANNEL_SPLICE_OUT, ROUTE_LDK_LIGHTNING_PROBE, ROUTE_LDK_LIGHTNING_RECEIVE,
    ROUTE_LDK_LIGHTNING_SEND, ROUTE_LDK_ONCHAIN_RECEIVE, ROUTE_LDK_ONCHAIN_SEND,
    ROUTE_LDK_PEER_CONNECT, ROUTE_LDK_PEER_DISCONNECT, ROUTE_LDK_PEER_LIST, ROUTE_MINT_ADD,
    ROUTE_MINT_BALANCE, ROUTE_MINT_CONFIG, ROUTE_MINT_LIST, ROUTE_MINT_MODULE_ECASH_COUNT,
    ROUTE_MINT_MODULE_ECASH_RECEIVE, ROUTE_MINT_MODULE_ECASH_SEND,
    ROUTE_MINT_MODULE_ONCHAIN_RECEIVE, ROUTE_MINT_MODULE_ONCHAIN_SEND,
    ROUTE_MINT_MODULE_ONCHAIN_SEND_FEE, ROUTE_MINT_REMOVE, ROUTE_MNEMONIC, ROUTE_QUERY,
};
use serde::Serialize;
use serde_json::Value;
use tokio::net::UnixStream;
use tower_service::Service;

#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Path to the gateway's data directory (must match the daemon's
    /// `DATA_DIR`). The CLI finds the admin Unix socket at
    /// `{DATA_DIR}/cli.sock`.
    #[arg(long = "data-dir", env = "DATA_DIR")]
    data_dir: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Display gateway info
    Info,
    /// Display mnemonic seed words
    Mnemonic,
    /// Query the analytics db with read-only SQL; rows print as JSON objects
    Query(QueryRequest),
    /// LDK lightning node management
    #[command(subcommand)]
    Ldk(LdkCommands),
    /// Mint management
    #[command(subcommand)]
    Mint(MintCommands),
}

#[derive(Subcommand)]
enum LdkCommands {
    /// Get node balances
    Balances,
    /// On-chain operations
    #[command(subcommand)]
    Onchain(LdkOnchainCommands),
    /// Channel operations
    #[command(subcommand)]
    Channel(LdkChannelCommands),
    /// Lightning operations
    #[command(subcommand)]
    Lightning(LdkLightningCommands),
    /// Peer management
    #[command(subcommand)]
    Peer(LdkPeerCommands),
}

#[derive(Subcommand)]
enum LdkOnchainCommands {
    /// Get a receive address
    Receive,
    /// Send funds
    Send(LdkOnchainSendRequest),
}

#[derive(Subcommand)]
enum LdkChannelCommands {
    /// Open a channel
    Open(LdkChannelOpenRequest),
    /// Close a channel
    Close(LdkChannelCloseRequest),
    /// List channels
    List,
    /// Splice on-chain funds into a channel (experimental)
    SpliceIn(LdkChannelSpliceInRequest),
    /// Splice funds out of a channel to an on-chain address (experimental)
    SpliceOut(LdkChannelSpliceOutRequest),
}

#[derive(Subcommand)]
enum LdkLightningCommands {
    /// Create a bolt11 invoice to receive a payment
    Receive(LdkLightningReceiveRequest),
    /// Pay a bolt11 invoice
    Send(LdkLightningSendRequest),
    /// Probe routes towards a node to warm the pathfinding scorer
    Probe(LdkLightningProbeRequest),
}

#[derive(Subcommand)]
enum LdkPeerCommands {
    /// Connect to a peer
    Connect(LdkPeerConnectRequest),
    /// Disconnect from a peer
    Disconnect(LdkPeerDisconnectRequest),
    /// List peers
    List,
}

#[derive(Subcommand)]
enum MintCommands {
    /// Add a mint
    Add(MintAddRequest),
    /// Remove a mint and delete all of its data. Destructive:
    /// check for in-flight payments via `query` first — failing to
    /// check might result in loss of funds.
    Remove(MintRemoveRequest),
    /// List connected mints
    List,
    /// Get a connected mint's JSON client config
    Config(MintConfigRequest),
    /// Get a mint's ecash balance
    Balance(MintBalanceRequest),
    /// Per-mint module commands
    #[command(subcommand)]
    Module(ModuleCommands),
}

#[derive(Subcommand)]
enum ModuleCommands {
    /// Ecash module commands
    #[command(subcommand)]
    Ecash(EcashCommands),
    /// Onchain module commands
    #[command(subcommand)]
    Onchain(OnchainCommands),
}

#[derive(Subcommand)]
enum EcashCommands {
    /// Count ecash notes by denomination
    Count(MintEcashCountRequest),
    /// Send ecash
    Send(MintEcashSendRequest),
    /// Receive ecash
    Receive(MintEcashReceiveRequest),
}

#[derive(Subcommand)]
enum OnchainCommands {
    /// Get send fee estimate
    SendFee(MintOnchainSendFeeRequest),
    /// Send onchain from the mint
    Send(MintOnchainSendRequest),
    /// Get receive address
    Receive(MintOnchainReceiveRequest),
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
            "Failed to POST {route} to gateway at {}",
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
        serde_json::from_slice(&resp_bytes).context("Failed to parse gateway response")
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
        Commands::Info => request(d, ROUTE_INFO, ()).await?,
        Commands::Mnemonic => request(d, ROUTE_MNEMONIC, ()).await?,
        Commands::Query(req) => request(d, ROUTE_QUERY, req).await?,

        Commands::Ldk(cmd) => match cmd {
            LdkCommands::Balances => request(d, ROUTE_LDK_BALANCES, ()).await?,
            LdkCommands::Onchain(cmd) => match cmd {
                LdkOnchainCommands::Receive => request(d, ROUTE_LDK_ONCHAIN_RECEIVE, ()).await?,
                LdkOnchainCommands::Send(req) => request(d, ROUTE_LDK_ONCHAIN_SEND, req).await?,
            },
            LdkCommands::Channel(cmd) => match cmd {
                LdkChannelCommands::Open(req) => request(d, ROUTE_LDK_CHANNEL_OPEN, req).await?,
                LdkChannelCommands::Close(req) => request(d, ROUTE_LDK_CHANNEL_CLOSE, req).await?,
                LdkChannelCommands::List => request(d, ROUTE_LDK_CHANNEL_LIST, ()).await?,
                LdkChannelCommands::SpliceIn(req) => {
                    request(d, ROUTE_LDK_CHANNEL_SPLICE_IN, req).await?
                }
                LdkChannelCommands::SpliceOut(req) => {
                    request(d, ROUTE_LDK_CHANNEL_SPLICE_OUT, req).await?
                }
            },
            LdkCommands::Lightning(cmd) => match cmd {
                LdkLightningCommands::Receive(req) => {
                    request(d, ROUTE_LDK_LIGHTNING_RECEIVE, req).await?
                }
                LdkLightningCommands::Send(req) => {
                    request(d, ROUTE_LDK_LIGHTNING_SEND, req).await?
                }
                LdkLightningCommands::Probe(req) => {
                    request(d, ROUTE_LDK_LIGHTNING_PROBE, req).await?
                }
            },
            LdkCommands::Peer(cmd) => match cmd {
                LdkPeerCommands::Connect(req) => request(d, ROUTE_LDK_PEER_CONNECT, req).await?,
                LdkPeerCommands::Disconnect(req) => {
                    request(d, ROUTE_LDK_PEER_DISCONNECT, req).await?
                }
                LdkPeerCommands::List => request(d, ROUTE_LDK_PEER_LIST, ()).await?,
            },
        },

        Commands::Mint(cmd) => match cmd {
            MintCommands::Add(req) => request(d, ROUTE_MINT_ADD, req).await?,
            MintCommands::Remove(req) => request(d, ROUTE_MINT_REMOVE, req).await?,
            MintCommands::List => request(d, ROUTE_MINT_LIST, ()).await?,
            MintCommands::Config(req) => request(d, ROUTE_MINT_CONFIG, req).await?,
            MintCommands::Balance(req) => request(d, ROUTE_MINT_BALANCE, req).await?,
            MintCommands::Module(cmd) => match cmd {
                ModuleCommands::Ecash(cmd) => match cmd {
                    EcashCommands::Count(req) => {
                        request(d, ROUTE_MINT_MODULE_ECASH_COUNT, req).await?
                    }
                    EcashCommands::Send(req) => {
                        request(d, ROUTE_MINT_MODULE_ECASH_SEND, req).await?
                    }
                    EcashCommands::Receive(req) => {
                        request(d, ROUTE_MINT_MODULE_ECASH_RECEIVE, req).await?
                    }
                },
                ModuleCommands::Onchain(cmd) => match cmd {
                    OnchainCommands::SendFee(req) => {
                        request(d, ROUTE_MINT_MODULE_ONCHAIN_SEND_FEE, req).await?
                    }
                    OnchainCommands::Send(req) => {
                        request(d, ROUTE_MINT_MODULE_ONCHAIN_SEND, req).await?
                    }
                    OnchainCommands::Receive(req) => {
                        request(d, ROUTE_MINT_MODULE_ONCHAIN_RECEIVE, req).await?
                    }
                },
            },
        },
    };

    print_json(&result);
    Ok(())
}
