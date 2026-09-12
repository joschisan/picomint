use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use picomint_cli_client::{print_json, request};
use picomint_node_cli_core::{
    ExpirySetRequest, InviteRequest, LightningGatewayAddRequest, LightningGatewayRemoveRequest,
    QueryRequest, ROUTE_BACKUP, ROUTE_EXPIRY_CLEAR, ROUTE_EXPIRY_SET, ROUTE_EXPIRY_STATUS,
    ROUTE_GATEWAY_ADD, ROUTE_GATEWAY_LIST, ROUTE_GATEWAY_REMOVE, ROUTE_INVITE,
    ROUTE_ONCHAIN_HISTORY, ROUTE_ONCHAIN_PENDING, ROUTE_ONCHAIN_STATUS, ROUTE_ONCHAIN_SWEEP,
    ROUTE_QUERY, ROUTE_SETUP_ADD, ROUTE_SETUP_CONFIRM, ROUTE_SETUP_INIT, ROUTE_SETUP_RESET,
    ROUTE_SETUP_RESTORE, ROUTE_STATUS, SetupAddRequest, SetupInitRequest,
};
use serde_json::Value;

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
    /// Which phase the node is in (setup, dkg, consensus) and what an operator needs at that point
    Status,
    /// The setup ceremony: init, exchange setup codes, confirm
    #[command(subcommand)]
    Setup(SetupCommands),
    /// Generate a mint invite code
    Invite(InviteRequest),
    /// The node config with its private keys, for `setup restore`; always pipe it into a file
    Backup,
    /// Query the analytics db with read-only SQL; rows print as JSON objects
    Query(QueryRequest),
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
    Set(ExpirySetRequest),
    /// Withdraw this node's announcement
    Clear,
    /// This node's announcement; clients trust it once a threshold of nodes agree
    Status,
}

#[derive(Subcommand)]
enum SetupCommands {
    /// Name this node and print its setup code for the other nodes
    Init(SetupInitRequest),
    /// Add a node's setup code
    Add(SetupAddRequest),
    /// Forget every added setup code and start collecting them again
    Reset,
    /// Confirm the node set; once every node has, key generation starts
    Confirm,
    /// Restore the node from a `backup.json` on stdin, skipping the ceremony
    Restore,
}

#[derive(Subcommand)]
enum OnchainCommands {
    /// The mint wallet at a glance: value, transaction tip and count, consensus fee rate
    Status,
    /// Mint transactions broadcast but not yet confirmed
    Pending,
    /// The mint's whole transaction history
    History,
    /// This node's sweep secret; run only once the mint has expired (secret)
    Sweep,
}

#[derive(Subcommand)]
enum GatewayCommands {
    /// Recommend a gateway; clients use it once a threshold of nodes do
    Add(LightningGatewayAddRequest),
    /// Withdraw this node's recommendation
    Remove(LightningGatewayRemoveRequest),
    /// The gateways this node recommends
    List,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let d = &cli.data_dir;

    let result = match cli.command {
        Commands::Status => request(d, ROUTE_STATUS, ()).await?,
        Commands::Invite(req) => request(d, ROUTE_INVITE, req).await?,
        Commands::Backup => request(d, ROUTE_BACKUP, ()).await?,
        Commands::Query(req) => request(d, ROUTE_QUERY, req).await?,

        Commands::Expiry(cmd) => match cmd {
            ExpiryCommands::Set(req) => request(d, ROUTE_EXPIRY_SET, req).await?,
            ExpiryCommands::Clear => request(d, ROUTE_EXPIRY_CLEAR, ()).await?,
            ExpiryCommands::Status => request(d, ROUTE_EXPIRY_STATUS, ()).await?,
        },

        Commands::Setup(cmd) => match cmd {
            SetupCommands::Init(req) => request(d, ROUTE_SETUP_INIT, req).await?,
            SetupCommands::Add(req) => request(d, ROUTE_SETUP_ADD, req).await?,
            SetupCommands::Reset => request(d, ROUTE_SETUP_RESET, ()).await?,
            SetupCommands::Confirm => request(d, ROUTE_SETUP_CONFIRM, ()).await?,
            SetupCommands::Restore => {
                let cfg: Value = serde_json::from_reader(std::io::stdin())?;
                request(d, ROUTE_SETUP_RESTORE, cfg).await?
            }
        },

        Commands::Onchain(cmd) => match cmd {
            OnchainCommands::Status => request(d, ROUTE_ONCHAIN_STATUS, ()).await?,
            OnchainCommands::Pending => request(d, ROUTE_ONCHAIN_PENDING, ()).await?,
            OnchainCommands::History => request(d, ROUTE_ONCHAIN_HISTORY, ()).await?,
            OnchainCommands::Sweep => request(d, ROUTE_ONCHAIN_SWEEP, ()).await?,
        },

        Commands::Gateway(cmd) => match cmd {
            GatewayCommands::Add(req) => request(d, ROUTE_GATEWAY_ADD, req).await?,
            GatewayCommands::Remove(req) => request(d, ROUTE_GATEWAY_REMOVE, req).await?,
            GatewayCommands::List => request(d, ROUTE_GATEWAY_LIST, ()).await?,
        },
    };

    print_json(&result);
    Ok(())
}
