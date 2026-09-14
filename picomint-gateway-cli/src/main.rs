use std::path::PathBuf;

use clap::{Parser, Subcommand};
use picomint_analytics::QueryError;
use picomint_cli_client::{FOOTER, print_json, request, schema, schema_fallible};
use picomint_client::ecash::{ReceiveEcashError, SendEcashError};
use picomint_client::{AddMintError, NotAddedError, onchain};
use picomint_gateway_cli_core::{
    ClientAddRequest, ClientBalanceRequest, ClientBalanceResponse, ClientConfigRequest,
    ClientConfigResponse, ClientEcashCountRequest, ClientEcashCountResponse,
    ClientEcashReceiveRequest, ClientEcashReceiveResponse, ClientEcashSendMaxRequest,
    ClientEcashSendMaxResponse, ClientEcashSendRequest, ClientEcashSendResponse,
    ClientListResponse, ClientOnchainReceiveRequest, ClientOnchainReceiveResponse,
    ClientOnchainSendFeeRequest, ClientOnchainSendFeeResponse, ClientOnchainSendMaxRequest,
    ClientOnchainSendMaxResponse, ClientOnchainSendRequest, ClientOnchainSendResponse,
    ClientRemoveRequest, InfoResponse, LdkBalancesResponse, LdkChannelCloseRequest,
    LdkChannelListResponse, LdkChannelOpenRequest, LdkChannelSpliceInRequest,
    LdkChannelSpliceOutRequest, LdkLightningProbeRequest, LdkLightningReceiveRequest,
    LdkLightningReceiveResponse, LdkLightningSendRequest, LdkLightningSendResponse,
    LdkOnchainReceiveResponse, LdkOnchainSendRequest, LdkOnchainSendResponse,
    LdkPeerConnectRequest, LdkPeerDisconnectRequest, LdkPeerListResponse, MnemonicResponse,
    QueryRequest, QueryResponse, ROUTE_CLIENT_ADD, ROUTE_CLIENT_BALANCE, ROUTE_CLIENT_CONFIG,
    ROUTE_CLIENT_ECASH_COUNT, ROUTE_CLIENT_ECASH_RECEIVE, ROUTE_CLIENT_ECASH_SEND,
    ROUTE_CLIENT_ECASH_SEND_MAX, ROUTE_CLIENT_LIST, ROUTE_CLIENT_ONCHAIN_RECEIVE,
    ROUTE_CLIENT_ONCHAIN_SEND, ROUTE_CLIENT_ONCHAIN_SEND_FEE, ROUTE_CLIENT_ONCHAIN_SEND_MAX,
    ROUTE_CLIENT_REMOVE, ROUTE_INFO, ROUTE_LDK_BALANCES, ROUTE_LDK_CHANNEL_CLOSE,
    ROUTE_LDK_CHANNEL_LIST, ROUTE_LDK_CHANNEL_OPEN, ROUTE_LDK_CHANNEL_SPLICE_IN,
    ROUTE_LDK_CHANNEL_SPLICE_OUT, ROUTE_LDK_LIGHTNING_PROBE, ROUTE_LDK_LIGHTNING_RECEIVE,
    ROUTE_LDK_LIGHTNING_SEND, ROUTE_LDK_ONCHAIN_RECEIVE, ROUTE_LDK_ONCHAIN_SEND,
    ROUTE_LDK_PEER_CONNECT, ROUTE_LDK_PEER_DISCONNECT, ROUTE_LDK_PEER_LIST, ROUTE_MNEMONIC,
    ROUTE_QUERY,
};
use picomint_gateway_cli_core::{LdkError, LdkReceiveError, LdkSendError};

#[derive(Parser)]
#[command(version, after_help = FOOTER)]
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
    /// LDK lightning node management
    #[command(subcommand)]
    Ldk(LdkCommands),
    /// The gateway as a client of its mints
    #[command(subcommand)]
    Client(ClientCommands),
}

#[derive(Subcommand)]
enum LdkCommands {
    /// Get node balances
    #[command(after_long_help = schema::<LdkBalancesResponse>())]
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
    #[command(after_long_help = schema_fallible::<LdkOnchainReceiveResponse, LdkError>())]
    Receive,
    /// Send funds
    #[command(after_long_help = schema_fallible::<LdkOnchainSendResponse, LdkError>())]
    Send(LdkOnchainSendRequest),
}

#[derive(Subcommand)]
enum LdkChannelCommands {
    /// Open a channel
    #[command(after_long_help = schema_fallible::<(), LdkError>())]
    Open(LdkChannelOpenRequest),
    /// Close a channel
    #[command(after_long_help = schema_fallible::<(), LdkError>())]
    Close(LdkChannelCloseRequest),
    /// List channels
    #[command(after_long_help = schema::<LdkChannelListResponse>())]
    List,
    /// Splice on-chain funds into a channel (experimental)
    #[command(after_long_help = schema_fallible::<(), LdkError>())]
    SpliceIn(LdkChannelSpliceInRequest),
    /// Splice funds out of a channel to an on-chain address (experimental)
    #[command(after_long_help = schema_fallible::<(), LdkError>())]
    SpliceOut(LdkChannelSpliceOutRequest),
}

#[derive(Subcommand)]
enum LdkLightningCommands {
    /// Create a bolt11 invoice to receive a payment
    #[command(after_long_help = schema_fallible::<LdkLightningReceiveResponse, LdkReceiveError>())]
    Receive(LdkLightningReceiveRequest),
    /// Pay a bolt11 invoice
    #[command(after_long_help = schema_fallible::<LdkLightningSendResponse, LdkSendError>())]
    Send(LdkLightningSendRequest),
    /// Probe routes towards a node to warm the pathfinding scorer
    #[command(after_long_help = schema_fallible::<(), LdkError>())]
    Probe(LdkLightningProbeRequest),
}

#[derive(Subcommand)]
enum LdkPeerCommands {
    /// Connect to a peer
    #[command(after_long_help = schema_fallible::<(), LdkError>())]
    Connect(LdkPeerConnectRequest),
    /// Disconnect from a peer
    #[command(after_long_help = schema_fallible::<(), LdkError>())]
    Disconnect(LdkPeerDisconnectRequest),
    /// List peers
    #[command(after_long_help = schema::<LdkPeerListResponse>())]
    List,
}

#[derive(Subcommand)]
enum ClientCommands {
    /// Add a mint
    #[command(after_long_help = schema_fallible::<(), AddMintError>())]
    Add(ClientAddRequest),
    /// Remove a mint and delete all of its data. Destructive:
    /// check for in-flight payments via `query` first — failing to
    /// check might result in loss of funds.
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
    /// Count ecash notes by denomination
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
    /// Send onchain from the mint
    #[command(after_long_help = schema_fallible::<ClientOnchainSendResponse, onchain::SendError>())]
    Send(ClientOnchainSendRequest),
    /// Send the account's entire balance onchain, minus the fee
    #[command(after_long_help = schema_fallible::<ClientOnchainSendMaxResponse, onchain::SendError>())]
    SendMax(ClientOnchainSendMaxRequest),
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

        Commands::Ldk(cmd) => match cmd {
            LdkCommands::Balances => request(d, ROUTE_LDK_BALANCES, ()).await,
            LdkCommands::Onchain(cmd) => match cmd {
                LdkOnchainCommands::Receive => request(d, ROUTE_LDK_ONCHAIN_RECEIVE, ()).await,
                LdkOnchainCommands::Send(req) => request(d, ROUTE_LDK_ONCHAIN_SEND, req).await,
            },
            LdkCommands::Channel(cmd) => match cmd {
                LdkChannelCommands::Open(req) => request(d, ROUTE_LDK_CHANNEL_OPEN, req).await,
                LdkChannelCommands::Close(req) => request(d, ROUTE_LDK_CHANNEL_CLOSE, req).await,
                LdkChannelCommands::List => request(d, ROUTE_LDK_CHANNEL_LIST, ()).await,
                LdkChannelCommands::SpliceIn(req) => {
                    request(d, ROUTE_LDK_CHANNEL_SPLICE_IN, req).await
                }
                LdkChannelCommands::SpliceOut(req) => {
                    request(d, ROUTE_LDK_CHANNEL_SPLICE_OUT, req).await
                }
            },
            LdkCommands::Lightning(cmd) => match cmd {
                LdkLightningCommands::Receive(req) => {
                    request(d, ROUTE_LDK_LIGHTNING_RECEIVE, req).await
                }
                LdkLightningCommands::Send(req) => request(d, ROUTE_LDK_LIGHTNING_SEND, req).await,
                LdkLightningCommands::Probe(req) => {
                    request(d, ROUTE_LDK_LIGHTNING_PROBE, req).await
                }
            },
            LdkCommands::Peer(cmd) => match cmd {
                LdkPeerCommands::Connect(req) => request(d, ROUTE_LDK_PEER_CONNECT, req).await,
                LdkPeerCommands::Disconnect(req) => {
                    request(d, ROUTE_LDK_PEER_DISCONNECT, req).await
                }
                LdkPeerCommands::List => request(d, ROUTE_LDK_PEER_LIST, ()).await,
            },
        },

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
