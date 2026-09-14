use std::path::PathBuf;

use clap::{Parser, Subcommand};
use picomint_cli_client::{FOOTER, RequestError, print_json, request, schema, schema_fallible};
use picomint_node_cli_core::{
    BackupResponse, BitcoindError, BitcoindResponse, ExpirySetError, ExpirySetRequest,
    ExpiryStatusResponse, GatewayAddError, GatewayRemoveError, HistoryResponse, InviteError,
    InviteRequest, InviteResponse, LightningGatewayAddRequest, LightningGatewayListResponse,
    LightningGatewayRemoveRequest, NodeStatus, OnchainStatusResponse, PendingResponse,
    ROUTE_BACKUP, ROUTE_BITCOIND, ROUTE_EXPIRY_CLEAR, ROUTE_EXPIRY_SET, ROUTE_EXPIRY_STATUS,
    ROUTE_GATEWAY_ADD, ROUTE_GATEWAY_LIST, ROUTE_GATEWAY_REMOVE, ROUTE_INVITE,
    ROUTE_ONCHAIN_HISTORY, ROUTE_ONCHAIN_PENDING, ROUTE_ONCHAIN_RUGPULL, ROUTE_ONCHAIN_STATUS,
    ROUTE_SETUP_ADD, ROUTE_SETUP_CONFIRM, ROUTE_SETUP_INIT, ROUTE_SETUP_RESET, ROUTE_SETUP_RESTORE,
    ROUTE_STATUS, RugpullError, RugpullResponse, SetupAddError, SetupAddRequest, SetupAddResponse,
    SetupConfirmError, SetupInitError, SetupInitRequest, SetupInitResponse, SetupRestoreError,
    StatusError,
};
use serde_json::Value;

/// Every route is served by exactly one phase, so an agent that gets this
/// code has a stale picture of the node, not a wrong command.
const PHASES: &str = "\
Every command is served by exactly one of the node's phases, setup, dkg \
and consensus; in any other phase it fails with the code wrong_phase, and \
`status` names the phase.";

#[derive(Parser)]
#[command(version, after_help = format!("{FOOTER}\n\n{PHASES}"))]
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
    /// Which phase the node is in (setup, dkg, consensus) and what an operator needs at that point
    #[command(after_long_help = schema_fallible::<NodeStatus, StatusError>())]
    Status,
    /// This node's own Bitcoin Core backend, read live: network, chain tip, fee estimate, sync progress
    #[command(after_long_help = schema_fallible::<BitcoindResponse, BitcoindError>())]
    Bitcoind,
    /// The setup ceremony: init, exchange setup codes, confirm
    #[command(subcommand)]
    Setup(SetupCommands),
    /// Generate a mint invite code
    #[command(after_long_help = schema_fallible::<InviteResponse, InviteError>())]
    Invite(InviteRequest),
    /// Print the node's whole config with its private keys; pipe it into a file (secret)
    #[command(after_long_help = schema::<BackupResponse>())]
    Backup,
    /// The mint's expiry announcement
    #[command(subcommand)]
    Expiry(ExpiryCommands),
    /// The mint wallet
    #[command(subcommand)]
    Onchain(OnchainCommands),
    /// The gateways this node recommends to clients
    #[command(subcommand)]
    Gateway(GatewayCommands),
}

#[derive(Subcommand)]
enum ExpiryCommands {
    /// Announce the mint's expiry; every node must enter the same values
    #[command(after_long_help = schema_fallible::<(), ExpirySetError>())]
    Set(ExpirySetRequest),
    /// Withdraw this node's announcement
    #[command(after_long_help = schema::<()>())]
    Clear,
    /// This node's announcement; clients trust it once a threshold of nodes agree
    #[command(after_long_help = schema::<ExpiryStatusResponse>())]
    Status,
}

#[derive(Subcommand)]
enum SetupCommands {
    /// Name this node and print its setup code for the other nodes
    #[command(after_long_help = schema_fallible::<SetupInitResponse, SetupInitError>())]
    Init(SetupInitRequest),
    /// Add a node's setup code
    #[command(after_long_help = schema_fallible::<SetupAddResponse, SetupAddError>())]
    Add(SetupAddRequest),
    /// Forget every added setup code and start collecting them again
    #[command(after_long_help = schema::<()>())]
    Reset,
    /// Confirm the node set; once every node has, key generation starts
    #[command(after_long_help = schema_fallible::<(), SetupConfirmError>())]
    Confirm,
    /// Restore the node from a `backup.json` on stdin, skipping the ceremony
    #[command(after_long_help = schema_fallible::<(), SetupRestoreError>())]
    Restore,
}

#[derive(Subcommand)]
enum OnchainCommands {
    /// The mint wallet at a glance: value, transaction tip and count, consensus fee rate
    #[command(after_long_help = schema::<OnchainStatusResponse>())]
    Status,
    /// Mint transactions broadcast but not yet confirmed
    #[command(after_long_help = schema::<PendingResponse>())]
    Pending,
    /// The mint's whole transaction history
    #[command(after_long_help = schema::<HistoryResponse>())]
    History,
    /// Print this node's rugpull secret, for draining the wallet once the mint has wound down; pipe it into a file (secret)
    #[command(after_long_help = schema_fallible::<RugpullResponse, RugpullError>())]
    Rugpull,
}

#[derive(Subcommand)]
enum GatewayCommands {
    /// Recommend a gateway; clients use it once a threshold of nodes do
    #[command(after_long_help = schema_fallible::<(), GatewayAddError>())]
    Add(LightningGatewayAddRequest),
    /// Withdraw this node's recommendation
    #[command(after_long_help = schema_fallible::<(), GatewayRemoveError>())]
    Remove(LightningGatewayRemoveRequest),
    /// The gateways this node recommends
    #[command(after_long_help = schema::<LightningGatewayListResponse>())]
    List,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();
    let d = &cli.data_dir;

    let result = match cli.command {
        Commands::Status => request(d, ROUTE_STATUS, ()).await,
        Commands::Bitcoind => request(d, ROUTE_BITCOIND, ()).await,
        Commands::Invite(req) => request(d, ROUTE_INVITE, req).await,
        Commands::Backup => request(d, ROUTE_BACKUP, ()).await,

        Commands::Expiry(cmd) => match cmd {
            ExpiryCommands::Set(req) => request(d, ROUTE_EXPIRY_SET, req).await,
            ExpiryCommands::Clear => request(d, ROUTE_EXPIRY_CLEAR, ()).await,
            ExpiryCommands::Status => request(d, ROUTE_EXPIRY_STATUS, ()).await,
        },

        Commands::Setup(cmd) => match cmd {
            SetupCommands::Init(req) => request(d, ROUTE_SETUP_INIT, req).await,
            SetupCommands::Add(req) => request(d, ROUTE_SETUP_ADD, req).await,
            SetupCommands::Reset => request(d, ROUTE_SETUP_RESET, ()).await,
            SetupCommands::Confirm => request(d, ROUTE_SETUP_CONFIRM, ()).await,
            SetupCommands::Restore => match serde_json::from_reader::<_, Value>(std::io::stdin()) {
                Ok(cfg) => request(d, ROUTE_SETUP_RESTORE, cfg).await,
                Err(e) => Err(RequestError::Usage(format!(
                    "stdin is not a JSON backup: {e}"
                ))),
            },
        },

        Commands::Onchain(cmd) => match cmd {
            OnchainCommands::Status => request(d, ROUTE_ONCHAIN_STATUS, ()).await,
            OnchainCommands::Pending => request(d, ROUTE_ONCHAIN_PENDING, ()).await,
            OnchainCommands::History => request(d, ROUTE_ONCHAIN_HISTORY, ()).await,
            OnchainCommands::Rugpull => request(d, ROUTE_ONCHAIN_RUGPULL, ()).await,
        },

        Commands::Gateway(cmd) => match cmd {
            GatewayCommands::Add(req) => request(d, ROUTE_GATEWAY_ADD, req).await,
            GatewayCommands::Remove(req) => request(d, ROUTE_GATEWAY_REMOVE, req).await,
            GatewayCommands::List => request(d, ROUTE_GATEWAY_LIST, ()).await,
        },
    };

    match result {
        Ok(value) => print_json(&value),
        Err(error) => error.exit(),
    }
}
