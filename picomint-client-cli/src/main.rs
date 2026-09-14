use std::path::PathBuf;

use clap::{Parser, Subcommand};
use picomint_analytics::QueryError;
use picomint_cli_client::{FOOTER, print_json, request, schema, schema_fallible};
use picomint_client::ecash::{ReceiveEcashError, SendEcashError};
use picomint_client::expiry::RefreshExpiryStatusError;
use picomint_client::lightning::{
    RefreshGatewaysError, SendMaxAmountError, SendMaxError, SendPaymentError,
};
use picomint_client::{AddMintError, NotAddedError, lightning, onchain};
use picomint_client_cli_core::{
    ClientAddRequest, ClientAddResponse, ClientBalanceRequest, ClientBalanceResponse,
    ClientConfigRequest, ClientConfigResponse, ClientEcashCountRequest, ClientEcashCountResponse,
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

/// The admin CLI of a picomint client daemon: a headless wallet holding
/// ecash in one or more mints.
///
/// Add a mint with an invite code; every per-mint command then takes the
/// mint id `add` printed and, where funds move, one of five accounts.
/// Commands that move funds return at once with an operation id; their
/// outcome lands in the analytics, read with `query`. Every command prints
/// JSON, and its --help ends with the schema of what it prints and the
/// codes it fails with.
#[derive(Parser)]
#[command(version, after_help = FOOTER)]
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
        "{}\n{}",
        picomint_analytics::tables(),
        schema_fallible::<QueryResponse, QueryError>()
    ))]
    Query(QueryRequest),
    /// Add a mint
    #[command(after_long_help = schema_fallible::<ClientAddResponse, AddMintError>())]
    Add(ClientAddRequest),
    /// Remove a mint and delete all of its data; destructive, so check for in-flight payments via `query` first
    #[command(after_long_help = schema_fallible::<(), NotAddedError>())]
    Remove(ClientRemoveRequest),
    /// List added mints
    #[command(after_long_help = schema::<ClientListResponse>())]
    List,
    /// Get a mint's JSON client config
    #[command(after_long_help = schema_fallible::<ClientConfigResponse, NotAddedError>())]
    Config(ClientConfigRequest),
    /// Fetch the mint's expiry announcement from its nodes
    #[command(after_long_help = schema_fallible::<ClientExpiryResponse, RefreshExpiryStatusError>())]
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
    /// Count ecash notes per denomination, keyed by the denomination's exponent
    #[command(after_long_help = schema::<ClientEcashCountResponse>())]
    Count(ClientEcashCountRequest),
    /// Send ecash
    #[command(after_long_help = schema_fallible::<ClientEcashSendResponse, SendEcashError>())]
    Send(ClientEcashSendRequest),
    /// Send the account's entire ecash balance
    #[command(after_long_help = schema_fallible::<ClientEcashSendMaxResponse, NotAddedError>())]
    SendMax(ClientEcashSendMaxRequest),
    /// Receive ecash
    #[command(after_long_help = schema_fallible::<ClientEcashReceiveResponse, ReceiveEcashError>())]
    Receive(ClientEcashReceiveRequest),
}

#[derive(Subcommand)]
enum OnchainCommands {
    /// Get send fee estimate
    #[command(after_long_help = schema_fallible::<ClientOnchainSendFeeResponse, onchain::SendFeeError>())]
    SendFee(ClientOnchainSendFeeRequest),
    /// Send onchain
    #[command(after_long_help = schema_fallible::<ClientOnchainSendResponse, onchain::SendError>())]
    Send(ClientOnchainSendRequest),
    /// What send-max would move right now
    #[command(after_long_help = schema_fallible::<ClientOnchainSendMaxAmountResponse, onchain::SendFeeError>())]
    SendMaxAmount(ClientOnchainSendMaxAmountRequest),
    /// Send the account's entire balance onchain, minus the fee
    #[command(after_long_help = schema_fallible::<ClientOnchainSendMaxResponse, onchain::SendError>())]
    SendMax(ClientOnchainSendMaxRequest),
    /// Get receive address
    #[command(after_long_help = schema_fallible::<ClientOnchainReceiveResponse, onchain::ReceiveError>())]
    Receive(ClientOnchainReceiveRequest),
}

#[derive(Subcommand)]
enum LightningCommands {
    /// The gateways the mint recommends, probed for their fees
    #[command(subcommand)]
    Gateway(LightningGatewayCommands),
    /// Pay a bolt11 invoice through a gateway
    #[command(after_long_help = schema_fallible::<ClientLightningSendResponse, SendPaymentError>())]
    Send(ClientLightningSendRequest),
    /// What send-max would pay right now
    #[command(after_long_help = schema_fallible::<ClientLightningSendMaxAmountResponse, SendMaxAmountError>())]
    SendMaxAmount(ClientLightningSendMaxAmountRequest),
    /// Empty an account to an lnurl
    #[command(after_long_help = schema_fallible::<ClientLightningSendMaxResponse, SendMaxError>())]
    SendMax(ClientLightningSendMaxRequest),
    /// Create a bolt11 invoice
    #[command(after_long_help = schema_fallible::<ClientLightningReceiveResponse, lightning::ReceiveError>())]
    Receive(ClientLightningReceiveRequest),
    /// Generate a shareable lnurl served by an lnurl daemon
    #[command(after_long_help = schema_fallible::<ClientLightningLnurlResponse, NotAddedError>())]
    Lnurl(ClientLightningLnurlRequest),
}

#[derive(Subcommand)]
enum LightningGatewayCommands {
    /// The gateways that answered a probe, keyed by pk, with their fees; filled when the mint is added
    #[command(after_long_help = schema_fallible::<ClientLightningGatewayListResponse, NotAddedError>())]
    List(ClientLightningGatewayListRequest),
    /// Re-fetch the mint's gateway list and re-probe every gateway
    #[command(after_long_help = schema_fallible::<ClientLightningGatewayListResponse, RefreshGatewaysError>())]
    Refresh(ClientLightningGatewayRefreshRequest),
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();
    let d = &cli.data_dir;

    let result = match cli.command {
        Commands::Mnemonic => request(d, ROUTE_MNEMONIC, ()).await,
        Commands::Query(req) => request(d, ROUTE_QUERY, req).await,
        Commands::Add(req) => request(d, ROUTE_ADD, req).await,
        Commands::Remove(req) => request(d, ROUTE_REMOVE, req).await,
        Commands::List => request(d, ROUTE_LIST, ()).await,
        Commands::Config(req) => request(d, ROUTE_CONFIG, req).await,
        Commands::Expiry(req) => request(d, ROUTE_EXPIRY, req).await,
        Commands::Balance(req) => request(d, ROUTE_BALANCE, req).await,
        Commands::Ecash(cmd) => match cmd {
            EcashCommands::Count(req) => request(d, ROUTE_ECASH_COUNT, req).await,
            EcashCommands::Send(req) => request(d, ROUTE_ECASH_SEND, req).await,
            EcashCommands::SendMax(req) => request(d, ROUTE_ECASH_SEND_MAX, req).await,
            EcashCommands::Receive(req) => request(d, ROUTE_ECASH_RECEIVE, req).await,
        },
        Commands::Onchain(cmd) => match cmd {
            OnchainCommands::SendFee(req) => request(d, ROUTE_ONCHAIN_SEND_FEE, req).await,
            OnchainCommands::Send(req) => request(d, ROUTE_ONCHAIN_SEND, req).await,
            OnchainCommands::SendMaxAmount(req) => {
                request(d, ROUTE_ONCHAIN_SEND_MAX_AMOUNT, req).await
            }
            OnchainCommands::SendMax(req) => request(d, ROUTE_ONCHAIN_SEND_MAX, req).await,
            OnchainCommands::Receive(req) => request(d, ROUTE_ONCHAIN_RECEIVE, req).await,
        },
        Commands::Lightning(cmd) => match cmd {
            LightningCommands::Gateway(cmd) => match cmd {
                LightningGatewayCommands::List(req) => {
                    request(d, ROUTE_LIGHTNING_GATEWAY_LIST, req).await
                }
                LightningGatewayCommands::Refresh(req) => {
                    request(d, ROUTE_LIGHTNING_GATEWAY_REFRESH, req).await
                }
            },
            LightningCommands::Send(req) => request(d, ROUTE_LIGHTNING_SEND, req).await,
            LightningCommands::SendMaxAmount(req) => {
                request(d, ROUTE_LIGHTNING_SEND_MAX_AMOUNT, req).await
            }
            LightningCommands::SendMax(req) => request(d, ROUTE_LIGHTNING_SEND_MAX, req).await,
            LightningCommands::Receive(req) => request(d, ROUTE_LIGHTNING_RECEIVE, req).await,
            LightningCommands::Lnurl(req) => request(d, ROUTE_LIGHTNING_LNURL, req).await,
        },
    };

    match result {
        Ok(value) => print_json(&value),
        Err(error) => error.exit(),
    }
}
