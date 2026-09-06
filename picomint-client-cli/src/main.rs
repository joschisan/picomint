use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use picomint_cli_client::{print_json, request};
use picomint_client_cli_core::{
    ClientAddRequest, ClientBalanceRequest, ClientConfigRequest, ClientEcashCountRequest,
    ClientEcashReceiveRequest, ClientEcashSendRequest, ClientLightningLnurlRequest,
    ClientLightningReceiveRequest, ClientLightningRefreshGatewaysRequest,
    ClientLightningSendMaxRequest, ClientLightningSendRequest, ClientOnchainReceiveRequest,
    ClientOnchainSendFeeRequest, ClientOnchainSendRequest, ClientRemoveRequest, QueryRequest,
    ROUTE_ADD, ROUTE_BALANCE, ROUTE_CONFIG, ROUTE_ECASH_COUNT, ROUTE_ECASH_RECEIVE,
    ROUTE_ECASH_SEND, ROUTE_LIGHTNING_LNURL, ROUTE_LIGHTNING_RECEIVE,
    ROUTE_LIGHTNING_REFRESH_GATEWAYS, ROUTE_LIGHTNING_SEND, ROUTE_LIGHTNING_SEND_MAX, ROUTE_LIST,
    ROUTE_MNEMONIC, ROUTE_ONCHAIN_RECEIVE, ROUTE_ONCHAIN_SEND, ROUTE_ONCHAIN_SEND_FEE, ROUTE_QUERY,
    ROUTE_REMOVE,
};

#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Path to the daemon's data directory (must match its `DATA_DIR`).
    /// The CLI finds the admin Unix socket at `{DATA_DIR}/cli.sock`.
    #[arg(long = "data-dir", env = "DATA_DIR")]
    data_dir: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Display mnemonic seed words
    Mnemonic,
    /// Query the analytics db with read-only SQL; rows print as JSON objects
    Query(QueryRequest),
    /// Add a mint
    Add(ClientAddRequest),
    /// Remove a mint and delete all of its data
    Remove(ClientRemoveRequest),
    /// List added mints
    List,
    /// Get a mint's JSON client config
    Config(ClientConfigRequest),
    /// Get an account's ecash balance
    Balance(ClientBalanceRequest),
    /// Ecash module commands
    #[command(subcommand)]
    Ecash(EcashCommands),
    /// Onchain module commands
    #[command(subcommand)]
    Onchain(OnchainCommands),
    /// Lightning module commands
    #[command(subcommand)]
    Lightning(LightningCommands),
}

#[derive(Subcommand)]
enum EcashCommands {
    /// Count ecash notes by denomination
    Count(ClientEcashCountRequest),
    /// Send ecash
    Send(ClientEcashSendRequest),
    /// Receive ecash
    Receive(ClientEcashReceiveRequest),
}

#[derive(Subcommand)]
enum OnchainCommands {
    /// Get send fee estimate
    SendFee(ClientOnchainSendFeeRequest),
    /// Send onchain
    Send(ClientOnchainSendRequest),
    /// Get receive address
    Receive(ClientOnchainReceiveRequest),
}

#[derive(Subcommand)]
enum LightningCommands {
    /// Pay a bolt11 invoice
    Send(ClientLightningSendRequest),
    /// Empty an account to an lnurl
    SendMax(ClientLightningSendMaxRequest),
    /// Create a bolt11 invoice
    Receive(ClientLightningReceiveRequest),
    /// Generate a shareable lnurl served by an lnurl daemon
    Lnurl(ClientLightningLnurlRequest),
    /// Re-fetch the mint's gateway list and re-probe every gateway
    RefreshGateways(ClientLightningRefreshGatewaysRequest),
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let d = &cli.data_dir;

    let result = match cli.command {
        Commands::Mnemonic => request(d, ROUTE_MNEMONIC, ()).await?,
        Commands::Query(req) => request(d, ROUTE_QUERY, req).await?,
        Commands::Add(req) => request(d, ROUTE_ADD, req).await?,
        Commands::Remove(req) => request(d, ROUTE_REMOVE, req).await?,
        Commands::List => request(d, ROUTE_LIST, ()).await?,
        Commands::Config(req) => request(d, ROUTE_CONFIG, req).await?,
        Commands::Balance(req) => request(d, ROUTE_BALANCE, req).await?,
        Commands::Ecash(cmd) => match cmd {
            EcashCommands::Count(req) => request(d, ROUTE_ECASH_COUNT, req).await?,
            EcashCommands::Send(req) => request(d, ROUTE_ECASH_SEND, req).await?,
            EcashCommands::Receive(req) => request(d, ROUTE_ECASH_RECEIVE, req).await?,
        },
        Commands::Onchain(cmd) => match cmd {
            OnchainCommands::SendFee(req) => request(d, ROUTE_ONCHAIN_SEND_FEE, req).await?,
            OnchainCommands::Send(req) => request(d, ROUTE_ONCHAIN_SEND, req).await?,
            OnchainCommands::Receive(req) => request(d, ROUTE_ONCHAIN_RECEIVE, req).await?,
        },
        Commands::Lightning(cmd) => match cmd {
            LightningCommands::Send(req) => request(d, ROUTE_LIGHTNING_SEND, req).await?,
            LightningCommands::SendMax(req) => request(d, ROUTE_LIGHTNING_SEND_MAX, req).await?,
            LightningCommands::Receive(req) => request(d, ROUTE_LIGHTNING_RECEIVE, req).await?,
            LightningCommands::Lnurl(req) => request(d, ROUTE_LIGHTNING_LNURL, req).await?,
            LightningCommands::RefreshGateways(req) => {
                request(d, ROUTE_LIGHTNING_REFRESH_GATEWAYS, req).await?
            }
        },
    };

    print_json(&result);
    Ok(())
}
