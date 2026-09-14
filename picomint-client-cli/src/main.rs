use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use picomint_cli_client::{print_json, request, schema};
use picomint_client_cli_core::{
    ClientAddRequest, ClientBalanceRequest, ClientBalanceResponse, ClientConfigRequest,
    ClientConfigResponse, ClientEcashCountRequest, ClientEcashCountResponse,
    ClientEcashReceiveRequest, ClientEcashReceiveResponse, ClientEcashSendMaxRequest,
    ClientEcashSendMaxResponse, ClientEcashSendRequest, ClientEcashSendResponse,
    ClientExpiryRequest, ClientExpiryResponse, ClientLightningGatewayListRequest,
    ClientLightningGatewayListResponse, ClientLightningGatewayRefreshRequest,
    ClientLightningLnurlRequest, ClientLightningLnurlResponse, ClientLightningReceiveRequest,
    ClientLightningReceiveResponse, ClientLightningSendMaxAmountRequest,
    ClientLightningSendMaxAmountResponse, ClientLightningSendMaxRequest,
    ClientLightningSendMaxResponse, ClientLightningSendRequest, ClientLightningSendResponse,
    ClientListResponse, ClientOnchainReceiveRequest, ClientOnchainReceiveResponse,
    ClientOnchainSendFeeRequest, ClientOnchainSendFeeResponse, ClientOnchainSendMaxAmountRequest,
    ClientOnchainSendMaxAmountResponse, ClientOnchainSendMaxRequest, ClientOnchainSendMaxResponse,
    ClientOnchainSendRequest, ClientOnchainSendResponse, ClientRemoveRequest, MnemonicResponse,
    QueryRequest, QueryResponse, ROUTE_ADD, ROUTE_BALANCE, ROUTE_CONFIG, ROUTE_ECASH_COUNT,
    ROUTE_ECASH_RECEIVE, ROUTE_ECASH_SEND, ROUTE_ECASH_SEND_MAX, ROUTE_EXPIRY,
    ROUTE_LIGHTNING_GATEWAY_LIST, ROUTE_LIGHTNING_GATEWAY_REFRESH, ROUTE_LIGHTNING_LNURL,
    ROUTE_LIGHTNING_RECEIVE, ROUTE_LIGHTNING_SEND, ROUTE_LIGHTNING_SEND_MAX,
    ROUTE_LIGHTNING_SEND_MAX_AMOUNT, ROUTE_LIST, ROUTE_MNEMONIC, ROUTE_ONCHAIN_RECEIVE,
    ROUTE_ONCHAIN_SEND, ROUTE_ONCHAIN_SEND_FEE, ROUTE_ONCHAIN_SEND_MAX,
    ROUTE_ONCHAIN_SEND_MAX_AMOUNT, ROUTE_QUERY, ROUTE_REMOVE,
};

/// Shown at the end of the top-level `--help`: the rules for an agent
/// driving this CLI, stated where the agent reads them.
const SECRETS: &str = "\
Commands marked (secret) print key material, and whatever an agent reads \
ends up in its context and transcript. Rules for an agent: run a (secret) \
command only when the operator asks; run it with stdout redirected into a \
file; never read that file; open it for the operator if asked.";

#[derive(Parser)]
#[command(version, after_help = SECRETS)]
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
    /// Print the mnemonic seed words; pipe it into a file (secret)
    #[command(after_long_help = schema::<MnemonicResponse>())]
    Mnemonic,
    /// Query the analytics db with read-only SQL; rows print as JSON objects
    #[command(after_long_help = format!(
        "Tables, as the SQL that creates them; every table is indexed on operation and ts:\n{}\n{}",
        picomint_analytics::tables(),
        schema::<QueryResponse>()
    ))]
    Query(QueryRequest),
    /// Add a mint
    #[command(after_long_help = schema::<()>())]
    Add(ClientAddRequest),
    /// Remove a mint and delete all of its data
    #[command(after_long_help = schema::<()>())]
    Remove(ClientRemoveRequest),
    /// List added mints
    #[command(after_long_help = schema::<ClientListResponse>())]
    List,
    /// Get a mint's JSON client config
    #[command(after_long_help = schema::<ClientConfigResponse>())]
    Config(ClientConfigRequest),
    /// Fetch the mint's expiry announcement from its nodes
    #[command(after_long_help = schema::<ClientExpiryResponse>())]
    Expiry(ClientExpiryRequest),
    /// Get an account's ecash balance
    #[command(after_long_help = schema::<ClientBalanceResponse>())]
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
    #[command(after_long_help = schema::<ClientEcashCountResponse>())]
    Count(ClientEcashCountRequest),
    /// Send ecash
    #[command(after_long_help = schema::<ClientEcashSendResponse>())]
    Send(ClientEcashSendRequest),
    /// Send the account's entire ecash balance
    #[command(after_long_help = schema::<ClientEcashSendMaxResponse>())]
    SendMax(ClientEcashSendMaxRequest),
    /// Receive ecash
    #[command(after_long_help = schema::<ClientEcashReceiveResponse>())]
    Receive(ClientEcashReceiveRequest),
}

#[derive(Subcommand)]
enum OnchainCommands {
    /// Get send fee estimate
    #[command(after_long_help = schema::<ClientOnchainSendFeeResponse>())]
    SendFee(ClientOnchainSendFeeRequest),
    /// Send onchain
    #[command(after_long_help = schema::<ClientOnchainSendResponse>())]
    Send(ClientOnchainSendRequest),
    /// What send-max would move right now
    #[command(after_long_help = schema::<ClientOnchainSendMaxAmountResponse>())]
    SendMaxAmount(ClientOnchainSendMaxAmountRequest),
    /// Send the account's entire balance onchain, minus the fee
    #[command(after_long_help = schema::<ClientOnchainSendMaxResponse>())]
    SendMax(ClientOnchainSendMaxRequest),
    /// Get receive address
    #[command(after_long_help = schema::<ClientOnchainReceiveResponse>())]
    Receive(ClientOnchainReceiveRequest),
}

#[derive(Subcommand)]
enum LightningCommands {
    /// The gateways the mint recommends, probed for their fees
    #[command(subcommand)]
    Gateway(LightningGatewayCommands),
    /// Pay a bolt11 invoice through a gateway
    #[command(after_long_help = schema::<ClientLightningSendResponse>())]
    Send(ClientLightningSendRequest),
    /// What send-max would pay right now
    #[command(after_long_help = schema::<ClientLightningSendMaxAmountResponse>())]
    SendMaxAmount(ClientLightningSendMaxAmountRequest),
    /// Empty an account to an lnurl
    #[command(after_long_help = schema::<ClientLightningSendMaxResponse>())]
    SendMax(ClientLightningSendMaxRequest),
    /// Create a bolt11 invoice
    #[command(after_long_help = schema::<ClientLightningReceiveResponse>())]
    Receive(ClientLightningReceiveRequest),
    /// Generate a shareable lnurl served by an lnurl daemon
    #[command(after_long_help = schema::<ClientLightningLnurlResponse>())]
    Lnurl(ClientLightningLnurlRequest),
}

#[derive(Subcommand)]
enum LightningGatewayCommands {
    /// The gateways that answered a probe, keyed by pk, with their fees
    #[command(after_long_help = schema::<ClientLightningGatewayListResponse>())]
    List(ClientLightningGatewayListRequest),
    /// Re-fetch the mint's gateway list and re-probe every gateway
    #[command(after_long_help = schema::<()>())]
    Refresh(ClientLightningGatewayRefreshRequest),
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
        Commands::Expiry(req) => request(d, ROUTE_EXPIRY, req).await?,
        Commands::Balance(req) => request(d, ROUTE_BALANCE, req).await?,
        Commands::Ecash(cmd) => match cmd {
            EcashCommands::Count(req) => request(d, ROUTE_ECASH_COUNT, req).await?,
            EcashCommands::Send(req) => request(d, ROUTE_ECASH_SEND, req).await?,
            EcashCommands::SendMax(req) => request(d, ROUTE_ECASH_SEND_MAX, req).await?,
            EcashCommands::Receive(req) => request(d, ROUTE_ECASH_RECEIVE, req).await?,
        },
        Commands::Onchain(cmd) => match cmd {
            OnchainCommands::SendFee(req) => request(d, ROUTE_ONCHAIN_SEND_FEE, req).await?,
            OnchainCommands::Send(req) => request(d, ROUTE_ONCHAIN_SEND, req).await?,
            OnchainCommands::SendMaxAmount(req) => {
                request(d, ROUTE_ONCHAIN_SEND_MAX_AMOUNT, req).await?
            }
            OnchainCommands::SendMax(req) => request(d, ROUTE_ONCHAIN_SEND_MAX, req).await?,
            OnchainCommands::Receive(req) => request(d, ROUTE_ONCHAIN_RECEIVE, req).await?,
        },
        Commands::Lightning(cmd) => match cmd {
            LightningCommands::Gateway(cmd) => match cmd {
                LightningGatewayCommands::List(req) => {
                    request(d, ROUTE_LIGHTNING_GATEWAY_LIST, req).await?
                }
                LightningGatewayCommands::Refresh(req) => {
                    request(d, ROUTE_LIGHTNING_GATEWAY_REFRESH, req).await?
                }
            },
            LightningCommands::Send(req) => request(d, ROUTE_LIGHTNING_SEND, req).await?,
            LightningCommands::SendMaxAmount(req) => {
                request(d, ROUTE_LIGHTNING_SEND_MAX_AMOUNT, req).await?
            }
            LightningCommands::SendMax(req) => request(d, ROUTE_LIGHTNING_SEND_MAX, req).await?,
            LightningCommands::Receive(req) => request(d, ROUTE_LIGHTNING_RECEIVE, req).await?,
            LightningCommands::Lnurl(req) => request(d, ROUTE_LIGHTNING_LNURL, req).await?,
        },
    };

    print_json(&result);
    Ok(())
}
