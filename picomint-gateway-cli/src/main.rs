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
    CLI_SOCKET_FILENAME, FederationBalanceRequest, FederationConfigRequest,
    FederationInviteRequest, FederationJoinRequest, FederationMintCountRequest,
    FederationMintReceiveRequest, FederationMintSendRequest, FederationWalletReceiveRequest,
    FederationWalletSendFeeRequest, FederationWalletSendRequest, LdkChannelCloseRequest,
    LdkChannelOpenRequest, LdkInvoiceCreateRequest, LdkInvoicePayRequest, LdkOnchainSendRequest,
    LdkPeerConnectRequest, LdkPeerDisconnectRequest, ROUTE_FEDERATION_BALANCE,
    ROUTE_FEDERATION_CONFIG, ROUTE_FEDERATION_INVITE, ROUTE_FEDERATION_JOIN, ROUTE_FEDERATION_LIST,
    ROUTE_FEDERATION_MODULE_MINT_COUNT, ROUTE_FEDERATION_MODULE_MINT_RECEIVE,
    ROUTE_FEDERATION_MODULE_MINT_SEND, ROUTE_FEDERATION_MODULE_WALLET_RECEIVE,
    ROUTE_FEDERATION_MODULE_WALLET_SEND, ROUTE_FEDERATION_MODULE_WALLET_SEND_FEE, ROUTE_INFO,
    ROUTE_LDK_BALANCES, ROUTE_LDK_CHANNEL_CLOSE, ROUTE_LDK_CHANNEL_LIST, ROUTE_LDK_CHANNEL_OPEN,
    ROUTE_LDK_INVOICE_CREATE, ROUTE_LDK_INVOICE_PAY, ROUTE_LDK_ONCHAIN_RECEIVE,
    ROUTE_LDK_ONCHAIN_SEND, ROUTE_LDK_PEER_CONNECT, ROUTE_LDK_PEER_DISCONNECT, ROUTE_LDK_PEER_LIST,
    ROUTE_MNEMONIC,
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
    /// LDK lightning node management
    #[command(subcommand)]
    Ldk(LdkCommands),
    /// Federation management
    #[command(subcommand)]
    Federation(FederationCommands),
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
    /// Invoice operations
    #[command(subcommand)]
    Invoice(LdkInvoiceCommands),
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
    /// Close channels with a peer
    Close(LdkChannelCloseRequest),
    /// List channels
    List,
}

#[derive(Subcommand)]
enum LdkInvoiceCommands {
    /// Create a bolt11 invoice
    Create(LdkInvoiceCreateRequest),
    /// Pay a bolt11 invoice
    Pay(LdkInvoicePayRequest),
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
enum FederationCommands {
    /// Join a federation
    Join(FederationJoinRequest),
    /// List connected federations
    List,
    /// Get a connected federation's JSON client config
    Config(FederationConfigRequest),
    /// Generate an invite code pointing at one guardian of one federation
    Invite(FederationInviteRequest),
    /// Get a federation's ecash balance
    Balance(FederationBalanceRequest),
    /// Per-federation module commands
    #[command(subcommand)]
    Module(ModuleCommands),
}

#[derive(Subcommand)]
enum ModuleCommands {
    /// Mint module commands
    #[command(subcommand)]
    Mint(MintCommands),
    /// Wallet module commands
    #[command(subcommand)]
    Wallet(WalletCommands),
}

#[derive(Subcommand)]
enum MintCommands {
    /// Count ecash notes by denomination
    Count(FederationMintCountRequest),
    /// Send ecash
    Send(FederationMintSendRequest),
    /// Receive ecash
    Receive(FederationMintReceiveRequest),
}

#[derive(Subcommand)]
enum WalletCommands {
    /// Get send fee estimate
    SendFee(FederationWalletSendFeeRequest),
    /// Send onchain from federation wallet
    Send(FederationWalletSendRequest),
    /// Get receive address
    Receive(FederationWalletReceiveRequest),
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
            },
            LdkCommands::Invoice(cmd) => match cmd {
                LdkInvoiceCommands::Create(req) => {
                    request(d, ROUTE_LDK_INVOICE_CREATE, req).await?
                }
                LdkInvoiceCommands::Pay(req) => request(d, ROUTE_LDK_INVOICE_PAY, req).await?,
            },
            LdkCommands::Peer(cmd) => match cmd {
                LdkPeerCommands::Connect(req) => request(d, ROUTE_LDK_PEER_CONNECT, req).await?,
                LdkPeerCommands::Disconnect(req) => {
                    request(d, ROUTE_LDK_PEER_DISCONNECT, req).await?
                }
                LdkPeerCommands::List => request(d, ROUTE_LDK_PEER_LIST, ()).await?,
            },
        },

        Commands::Federation(cmd) => match cmd {
            FederationCommands::Join(req) => request(d, ROUTE_FEDERATION_JOIN, req).await?,
            FederationCommands::List => request(d, ROUTE_FEDERATION_LIST, ()).await?,
            FederationCommands::Config(req) => request(d, ROUTE_FEDERATION_CONFIG, req).await?,
            FederationCommands::Invite(req) => request(d, ROUTE_FEDERATION_INVITE, req).await?,
            FederationCommands::Balance(req) => request(d, ROUTE_FEDERATION_BALANCE, req).await?,
            FederationCommands::Module(cmd) => match cmd {
                ModuleCommands::Mint(cmd) => match cmd {
                    MintCommands::Count(req) => {
                        request(d, ROUTE_FEDERATION_MODULE_MINT_COUNT, req).await?
                    }
                    MintCommands::Send(req) => {
                        request(d, ROUTE_FEDERATION_MODULE_MINT_SEND, req).await?
                    }
                    MintCommands::Receive(req) => {
                        request(d, ROUTE_FEDERATION_MODULE_MINT_RECEIVE, req).await?
                    }
                },
                ModuleCommands::Wallet(cmd) => match cmd {
                    WalletCommands::SendFee(req) => {
                        request(d, ROUTE_FEDERATION_MODULE_WALLET_SEND_FEE, req).await?
                    }
                    WalletCommands::Send(req) => {
                        request(d, ROUTE_FEDERATION_MODULE_WALLET_SEND, req).await?
                    }
                    WalletCommands::Receive(req) => {
                        request(d, ROUTE_FEDERATION_MODULE_WALLET_RECEIVE, req).await?
                    }
                },
            },
        },
    };

    print_json(&result);
    Ok(())
}
