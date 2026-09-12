use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use picomint_cli_client::{print_json, request};
use picomint_node_cli_core::{
    ExpirySetRequest, InviteRequest, LightningGatewayAddRequest, LightningGatewayRemoveRequest,
    ROUTE_BITCOIN_CONNECTION, ROUTE_BLOCK_HEIGHT, ROUTE_CONFIG, ROUTE_EXPIRY_CLEAR,
    ROUTE_EXPIRY_SET, ROUTE_EXPIRY_STATUS, ROUTE_INVITE, ROUTE_MODULE_LN_GATEWAY_ADD,
    ROUTE_MODULE_LN_GATEWAY_LIST, ROUTE_MODULE_LN_GATEWAY_REMOVE, ROUTE_MODULE_ONCHAIN_FEERATE,
    ROUTE_MODULE_ONCHAIN_PENDING_TXS, ROUTE_MODULE_ONCHAIN_STATUS, ROUTE_MODULE_ONCHAIN_SWEEP,
    ROUTE_MODULE_ONCHAIN_TOTAL_VALUE, ROUTE_MODULE_ONCHAIN_TXS, ROUTE_P2P, ROUTE_SESSION_COUNT,
    ROUTE_SETUP_ADD_NODE, ROUTE_SETUP_CONFIRM, ROUTE_SETUP_INIT, ROUTE_SETUP_RESET,
    ROUTE_SETUP_RESTORE, ROUTE_SETUP_STATUS, ROUTE_STATUS, SetupAddNodeRequest, SetupInitRequest,
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
    /// Setup commands (DKG)
    #[command(subcommand)]
    Setup(SetupCommands),
    /// Generate a mint invite code
    Invite(InviteRequest),
    /// Dump full node config as JSON (use `> config.json` to save)
    Config,
    /// Number of consensus sessions this node has finalized
    SessionCount,
    /// Get the mint's consensus block height
    BlockHeight,
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
    /// Initialize this node and print its setup code
    Init(SetupInitRequest),
    /// Add a node's setup code
    AddNode(SetupAddNodeRequest),
    /// Forget every added setup code and start collecting them again
    Reset,
    /// Confirm the node set; once every node has, key generation starts
    Confirm,
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
    /// The mint wallet at a glance: value, transaction tip and count, consensus fee rate, pending transactions
    Status,
    /// Get total onchain value
    TotalValue,
    /// Get consensus fee rate
    Feerate,
    /// Get pending transactions
    PendingTxs,
    /// Get transactions
    Txs,
    /// Export the tweaked keys that sweep the mint wallet after decommissioning (secret)
    Sweep,
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

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let d = &cli.data_dir;

    let result = match cli.command {
        Commands::Status => request(d, ROUTE_STATUS, ()).await?,
        Commands::Invite(req) => request(d, ROUTE_INVITE, req).await?,
        Commands::Config => request(d, ROUTE_CONFIG, ()).await?,
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
            SetupCommands::Status => request(d, ROUTE_SETUP_STATUS, ()).await?,
            SetupCommands::Init(req) => request(d, ROUTE_SETUP_INIT, req).await?,
            SetupCommands::AddNode(req) => request(d, ROUTE_SETUP_ADD_NODE, req).await?,
            SetupCommands::Reset => request(d, ROUTE_SETUP_RESET, ()).await?,
            SetupCommands::Confirm => request(d, ROUTE_SETUP_CONFIRM, ()).await?,
            SetupCommands::Restore { path } => {
                let bytes = std::fs::read(&path)?;
                let cfg: Value = serde_json::from_slice(&bytes)?;
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
