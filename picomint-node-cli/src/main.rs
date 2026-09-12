use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use picomint_cli_client::{print_json, request};
use picomint_node_cli_core::{
    ExpirySetRequest, InviteRequest, LightningGatewayAddRequest, LightningGatewayRemoveRequest,
    ROUTE_BACKUP, ROUTE_BITCOIN_CONNECTION, ROUTE_BLOCK_HEIGHT, ROUTE_EXPIRY_CLEAR,
    ROUTE_EXPIRY_SET, ROUTE_EXPIRY_STATUS, ROUTE_INVITE, ROUTE_MODULE_LN_GATEWAY_ADD,
    ROUTE_MODULE_LN_GATEWAY_LIST, ROUTE_MODULE_LN_GATEWAY_REMOVE, ROUTE_MODULE_ONCHAIN_FEERATE,
    ROUTE_MODULE_ONCHAIN_PENDING_TXS, ROUTE_MODULE_ONCHAIN_STATUS, ROUTE_MODULE_ONCHAIN_SWEEP,
    ROUTE_MODULE_ONCHAIN_TOTAL_VALUE, ROUTE_MODULE_ONCHAIN_TXS, ROUTE_P2P, ROUTE_SESSION_COUNT,
    ROUTE_SETUP_ADD_NODE, ROUTE_SETUP_CONFIRM, ROUTE_SETUP_INIT, ROUTE_SETUP_RESET,
    ROUTE_SETUP_RESTORE, ROUTE_STATUS, SetupAddNodeRequest, SetupInitRequest,
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
    /// Number of consensus sessions this node has finalized; also in `status`
    SessionCount,
    /// The mint's consensus block height; also in `status`
    BlockHeight,
    /// Every peer's connection; also in `status`
    P2p,
    /// The local bitcoin backend; also in `status`
    BitcoinConnection,
    /// The mint's expiry announcement
    #[command(subcommand)]
    Expiry(ExpiryCommands),
    /// Module admin commands
    #[command(subcommand)]
    Module(ModuleCommands),
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
    AddNode(SetupAddNodeRequest),
    /// Forget every added setup code and start collecting them again
    Reset,
    /// Confirm the node set; once every node has, key generation starts
    Confirm,
    /// Restore the node from a `backup.json` on stdin, skipping the ceremony
    Restore,
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
    /// The mint wallet at a glance: value, transaction tip and count, consensus fee rate, pending transactions
    Status,
    /// The value in custody in sat; also in `module onchain status`
    TotalValue,
    /// The consensus fee rate in sat/vB; also in `module onchain status`
    Feerate,
    /// Mint transactions broadcast but not yet confirmed; also in `module onchain status`
    PendingTxs,
    /// The mint's whole transaction history
    Txs,
    /// This node's sweep secret; run only once the mint has expired (secret)
    Sweep,
}

#[derive(Subcommand)]
enum LightningCommands {
    /// The gateways this node recommends to clients
    #[command(subcommand)]
    Gateway(LightningGatewayCommands),
}

#[derive(Subcommand)]
enum LightningGatewayCommands {
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
        Commands::SessionCount => request(d, ROUTE_SESSION_COUNT, ()).await?,
        Commands::BlockHeight => request(d, ROUTE_BLOCK_HEIGHT, ()).await?,
        Commands::P2p => request(d, ROUTE_P2P, ()).await?,
        Commands::BitcoinConnection => request(d, ROUTE_BITCOIN_CONNECTION, ()).await?,

        Commands::Expiry(cmd) => match cmd {
            ExpiryCommands::Set(req) => request(d, ROUTE_EXPIRY_SET, req).await?,
            ExpiryCommands::Clear => request(d, ROUTE_EXPIRY_CLEAR, ()).await?,
            ExpiryCommands::Status => request(d, ROUTE_EXPIRY_STATUS, ()).await?,
        },

        Commands::Setup(cmd) => match cmd {
            SetupCommands::Init(req) => request(d, ROUTE_SETUP_INIT, req).await?,
            SetupCommands::AddNode(req) => request(d, ROUTE_SETUP_ADD_NODE, req).await?,
            SetupCommands::Reset => request(d, ROUTE_SETUP_RESET, ()).await?,
            SetupCommands::Confirm => request(d, ROUTE_SETUP_CONFIRM, ()).await?,
            SetupCommands::Restore => {
                let cfg: Value = serde_json::from_reader(std::io::stdin())?;
                request(d, ROUTE_SETUP_RESTORE, cfg).await?
            }
        },

        Commands::Module(cmd) => match cmd {
            ModuleCommands::Onchain(cmd) => match cmd {
                OnchainCommands::Status => request(d, ROUTE_MODULE_ONCHAIN_STATUS, ()).await?,
                OnchainCommands::TotalValue => {
                    request(d, ROUTE_MODULE_ONCHAIN_TOTAL_VALUE, ()).await?
                }
                OnchainCommands::Feerate => request(d, ROUTE_MODULE_ONCHAIN_FEERATE, ()).await?,
                OnchainCommands::PendingTxs => {
                    request(d, ROUTE_MODULE_ONCHAIN_PENDING_TXS, ()).await?
                }
                OnchainCommands::Txs => request(d, ROUTE_MODULE_ONCHAIN_TXS, ()).await?,
                OnchainCommands::Sweep => request(d, ROUTE_MODULE_ONCHAIN_SWEEP, ()).await?,
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
