use std::path::PathBuf;

use clap::{Parser, Subcommand};
use picomint_analytics::QueryError;
use picomint_broker_cli_core::{
    ClientAddRequest, ClientAddResponse, ClientBalanceRequest, ClientBalanceResponse,
    ClientConfigRequest, ClientConfigResponse, ClientEcashCountRequest, ClientEcashCountResponse,
    ClientEcashReceiveRequest, ClientEcashReceiveResponse, ClientEcashSendMaxRequest,
    ClientEcashSendMaxResponse, ClientEcashSendRequest, ClientEcashSendResponse,
    ClientListResponse, ClientOnchainReceiveFeeRequest, ClientOnchainReceiveFeeResponse,
    ClientOnchainReceiveRequest, ClientOnchainReceiveResponse, ClientOnchainSendFeeRequest,
    ClientOnchainSendFeeResponse, ClientOnchainSendMaxRequest, ClientOnchainSendMaxResponse,
    ClientOnchainSendRequest, ClientOnchainSendResponse, ClientRemoveRequest, InfoResponse,
    MnemonicResponse, QueryRequest, QueryResponse, ROUTE_CLIENT_ADD, ROUTE_CLIENT_BALANCE,
    ROUTE_CLIENT_CONFIG, ROUTE_CLIENT_ECASH_COUNT, ROUTE_CLIENT_ECASH_RECEIVE,
    ROUTE_CLIENT_ECASH_SEND, ROUTE_CLIENT_ECASH_SEND_MAX, ROUTE_CLIENT_LIST,
    ROUTE_CLIENT_ONCHAIN_RECEIVE, ROUTE_CLIENT_ONCHAIN_RECEIVE_FEE, ROUTE_CLIENT_ONCHAIN_SEND,
    ROUTE_CLIENT_ONCHAIN_SEND_FEE, ROUTE_CLIENT_ONCHAIN_SEND_MAX, ROUTE_CLIENT_REMOVE, ROUTE_INFO,
    ROUTE_MNEMONIC, ROUTE_QUERY, ROUTE_REBALANCE, RebalanceError, RebalanceResponse,
};
use picomint_cli_client::{FOOTER, print_json, request, schema, schema_fallible};
use picomint_client::ecash::{ReceiveEcashError, SendEcashError};
use picomint_client::{AddMintError, NotAddedError, onchain};

/// The admin CLI of a picomint broker: a client of several mints that
/// funds swaps between them for a fee.
///
/// `client` manages the broker's balances in its mints. Run `info` first
/// for its identity: a mint's nodes recommend the broker by its
/// `broker_pk`, and `client add` joins it to a mint. Every command prints
/// JSON, and its --help ends with the schema of what it prints and the
/// codes it fails with.
#[derive(Parser)]
#[command(version, after_help = FOOTER)]
struct Cli {
    /// Path to the broker's data directory (must match the daemon's
    /// `DATA_DIR`). The CLI finds the admin Unix socket at
    /// `{DATA_DIR}/cli.sock`.
    #[arg(long = "data-dir", env = "DATA_DIR")]
    data_dir: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Display broker info
    #[command(after_long_help = schema::<InfoResponse>())]
    Info,
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
    /// Move the highest mint balance towards the mean and the lowest onto it onchain, if it pays for itself
    #[command(after_long_help = schema_fallible::<Option<RebalanceResponse>, RebalanceError>())]
    Rebalance,
    /// The broker as a client of its mints
    #[command(subcommand)]
    Client(ClientCommands),
}

#[derive(Subcommand)]
enum ClientCommands {
    /// Add a mint
    #[command(after_long_help = schema_fallible::<ClientAddResponse, AddMintError>())]
    Add(ClientAddRequest),
    /// Remove a mint and delete all of its data; destructive, so check for in-flight swaps via `query` first
    #[command(after_long_help = schema_fallible::<(), NotAddedError>())]
    Remove(ClientRemoveRequest),
    /// List connected mints
    #[command(after_long_help = schema::<ClientListResponse>())]
    List,
    /// Get a connected mint's JSON client config
    #[command(after_long_help = schema_fallible::<ClientConfigResponse, NotAddedError>())]
    Config(ClientConfigRequest),
    /// Get a mint's ecash balance
    #[command(after_long_help = schema::<ClientBalanceResponse>())]
    Balance(ClientBalanceRequest),
    /// Ecash module commands
    #[command(subcommand)]
    Ecash(EcashCommands),
    /// Onchain module commands
    #[command(subcommand)]
    Onchain(OnchainCommands),
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
    #[command(after_long_help = schema_fallible::<ClientOnchainSendFeeResponse, onchain::FeeError>())]
    SendFee(ClientOnchainSendFeeRequest),
    /// Send onchain from the mint
    #[command(after_long_help = schema_fallible::<ClientOnchainSendResponse, onchain::SendError>())]
    Send(ClientOnchainSendRequest),
    /// Send the account's entire balance onchain, minus the fee
    #[command(after_long_help = schema_fallible::<ClientOnchainSendMaxResponse, onchain::SendError>())]
    SendMax(ClientOnchainSendMaxRequest),
    /// What the mint takes out of a deposit to sweep it
    #[command(after_long_help = schema_fallible::<ClientOnchainReceiveFeeResponse, onchain::FeeError>())]
    ReceiveFee(ClientOnchainReceiveFeeRequest),
    /// Get receive address
    #[command(after_long_help = schema_fallible::<ClientOnchainReceiveResponse, onchain::ReceiveError>())]
    Receive(ClientOnchainReceiveRequest),
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();
    let d = &cli.data_dir;

    let result = match cli.command {
        Commands::Info => request(d, ROUTE_INFO, ()).await,
        Commands::Mnemonic => request(d, ROUTE_MNEMONIC, ()).await,
        Commands::Query(req) => request(d, ROUTE_QUERY, req).await,
        Commands::Rebalance => request(d, ROUTE_REBALANCE, ()).await,

        Commands::Client(cmd) => match cmd {
            ClientCommands::Add(req) => request(d, ROUTE_CLIENT_ADD, req).await,
            ClientCommands::Remove(req) => request(d, ROUTE_CLIENT_REMOVE, req).await,
            ClientCommands::List => request(d, ROUTE_CLIENT_LIST, ()).await,
            ClientCommands::Config(req) => request(d, ROUTE_CLIENT_CONFIG, req).await,
            ClientCommands::Balance(req) => request(d, ROUTE_CLIENT_BALANCE, req).await,
            ClientCommands::Ecash(cmd) => match cmd {
                EcashCommands::Count(req) => request(d, ROUTE_CLIENT_ECASH_COUNT, req).await,
                EcashCommands::Send(req) => request(d, ROUTE_CLIENT_ECASH_SEND, req).await,
                EcashCommands::SendMax(req) => request(d, ROUTE_CLIENT_ECASH_SEND_MAX, req).await,
                EcashCommands::Receive(req) => request(d, ROUTE_CLIENT_ECASH_RECEIVE, req).await,
            },
            ClientCommands::Onchain(cmd) => match cmd {
                OnchainCommands::SendFee(req) => {
                    request(d, ROUTE_CLIENT_ONCHAIN_SEND_FEE, req).await
                }
                OnchainCommands::Send(req) => request(d, ROUTE_CLIENT_ONCHAIN_SEND, req).await,
                OnchainCommands::SendMax(req) => {
                    request(d, ROUTE_CLIENT_ONCHAIN_SEND_MAX, req).await
                }
                OnchainCommands::ReceiveFee(req) => {
                    request(d, ROUTE_CLIENT_ONCHAIN_RECEIVE_FEE, req).await
                }
                OnchainCommands::Receive(req) => {
                    request(d, ROUTE_CLIENT_ONCHAIN_RECEIVE, req).await
                }
            },
        },
    };

    match result {
        Ok(value) => print_json(&value),
        Err(error) => error.exit(),
    }
}
