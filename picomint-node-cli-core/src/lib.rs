use clap::Args;
use picomint_core::NodeId;
use picomint_core::config::MintId;
use picomint_core::expiry::ExpiryStatus;
use picomint_core::invite::InviteCode;
use picomint_core::onchain::{MintUtxo, TxInfo};
use picomint_core::version::ConsensusVersion;
use serde::{Deserialize, Serialize};

/// Filename of the node's admin CLI Unix socket, inside `DATA_DIR`.
/// Status of the setup flow, as reported by `/setup/status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SetupStatus {
    AwaitingInit,
    SharingSetupCodes,
}

/// Served in every phase of the node's life; everything else is phase-bound.
pub const ROUTE_STATUS: &str = "/status";

// Setup routes
pub const ROUTE_SETUP_STATUS: &str = "/setup/status";
pub const ROUTE_SETUP_INIT: &str = "/setup/init";
pub const ROUTE_SETUP_ADD_NODE: &str = "/setup/add-node";
pub const ROUTE_SETUP_RESET: &str = "/setup/reset";
pub const ROUTE_SETUP_START_DKG: &str = "/setup/start-dkg";
pub const ROUTE_SETUP_RESTORE: &str = "/setup/restore";

// Dashboard routes
pub const ROUTE_INVITE: &str = "/invite";
pub const ROUTE_CONFIG: &str = "/config";
pub const ROUTE_SESSION_COUNT: &str = "/session-count";
pub const ROUTE_BLOCK_HEIGHT: &str = "/block-height";
pub const ROUTE_P2P: &str = "/p2p";
pub const ROUTE_BITCOIN_CONNECTION: &str = "/bitcoin-connection";
pub const ROUTE_EXPIRY_SET: &str = "/expiry/set";
pub const ROUTE_EXPIRY_CLEAR: &str = "/expiry/clear";
pub const ROUTE_EXPIRY_STATUS: &str = "/expiry/status";

// Module routes
pub const ROUTE_MODULE_ONCHAIN_TOTAL_VALUE: &str = "/module/onchain/total-value";
pub const ROUTE_MODULE_ONCHAIN_FEERATE: &str = "/module/onchain/feerate";
pub const ROUTE_MODULE_ONCHAIN_PENDING_TXS: &str = "/module/onchain/pending-txs";
pub const ROUTE_MODULE_ONCHAIN_TXS: &str = "/module/onchain/txs";
pub const ROUTE_MODULE_ONCHAIN_SWEEP: &str = "/module/onchain/sweep";
pub const ROUTE_MODULE_LN_GATEWAY_ADD: &str = "/module/lightning/gateway/add";
pub const ROUTE_MODULE_LN_GATEWAY_REMOVE: &str = "/module/lightning/gateway/remove";
pub const ROUTE_MODULE_LN_GATEWAY_LIST: &str = "/module/lightning/gateway/list";

// --- /status ---

/// Which phase the node is in, with what an operator needs at that point.
/// The variant is the first thing to look at: every other route is served
/// by exactly one phase.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "phase")]
pub enum NodeStatus {
    Setup(SetupPhase),
    Dkg(DkgPhase),
    Consensus(Box<ConsensusPhase>),
}

/// Setup phase: the ceremony state as this node sees it. `setup_code` is
/// `None` until `setup init` has run; `mint_name` and `mint_size` are set
/// once any node's setup code has carried them.
#[derive(Debug, Serialize, Deserialize)]
pub struct SetupPhase {
    pub setup_code: Option<String>,
    pub node_name: Option<String>,
    pub mint_name: Option<String>,
    pub mint_size: Option<u8>,
    /// Names of the other nodes whose setup codes have been added.
    pub nodes: Vec<String>,
}

/// DKG phase: key generation is running; nothing else can be done until it
/// completes and the node moves to consensus.
#[derive(Debug, Serialize, Deserialize)]
pub struct DkgPhase {
    pub setup_code: String,
}

/// Consensus phase: the mint is running. Everything here is public; the
/// private keys are only ever returned by `config`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ConsensusPhase {
    pub mint_name: String,
    pub mint_id: MintId,
    pub network: String,
    pub node_id: NodeId,
    pub node_name: String,
    pub consensus_version: ConsensusVersion,
    pub session_count: u32,
    pub block_height: u32,
    pub value_in_custody_sat: Option<u64>,
    /// The mint's current wallet UTXO. Its tweak is what a sweep after
    /// decommissioning needs alongside the backup, so record it before the
    /// last node goes down.
    pub mint_utxo: Option<MintUtxo>,
    pub nodes: Vec<NodeInfo>,
    pub bitcoin: Option<BitcoinConnectionResponse>,
    pub expiry: Option<ExpiryStatus>,
}

// --- /setup/status ---
// Response: SetupStatus (defined above)

// --- /setup/init ---

#[derive(Clone, Debug, Serialize, Deserialize, Args)]
pub struct SetupInitRequest {
    /// Node name
    pub name: String,
    /// Mint name (leader only)
    #[arg(long)]
    pub mint_name: Option<String>,
    /// Mint size (leader only)
    #[arg(long)]
    pub mint_size: Option<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SetupInitResponse {
    pub setup_code: String,
}

// --- /setup/add-node ---

#[derive(Clone, Debug, Serialize, Deserialize, Args)]
pub struct SetupAddNodeRequest {
    /// Node's setup code
    pub setup_code: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SetupAddNodeResponse {
    pub name: String,
}

// --- /setup/start-dkg ---
// No request/response types (unit)

// --- /invite ---

/// Default invite-code expiration, in days.
pub const DEFAULT_INVITE_EXPIRY_DAYS: u64 = 30;

/// Maximum expiration of an invite code in days. One year is far below
/// chrono's date-range limit, so the expiry arithmetic in
/// `create_invite_code` cannot overflow.
pub const INVITE_EXPIRY_DAYS_LIMIT: u64 = 365;

/// Default number of users an invite code may onboard.
pub const DEFAULT_INVITE_USER_LIMIT: u32 = 50;

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct InviteRequest {
    /// Days until the invite code expires.
    #[arg(long = "days", default_value_t = DEFAULT_INVITE_EXPIRY_DAYS)]
    pub expiry_days: u64,
    /// Maximum number of users that may onboard with this invite code.
    #[arg(long = "users", default_value_t = DEFAULT_INVITE_USER_LIMIT)]
    pub user_limit: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InviteResponse {
    pub invite: InviteCode,
}

// --- /module/onchain/total-value ---

#[derive(Debug, Serialize, Deserialize)]
pub struct OnchainTotalValueResponse {
    pub total_value_sat: Option<u64>,
}

// --- /block-height ---

#[derive(Debug, Serialize, Deserialize)]
pub struct BlockHeightResponse {
    pub block_height: u32,
}

// --- /p2p ---

#[derive(Debug, Serialize, Deserialize)]
pub struct P2pResponse {
    pub nodes: Vec<NodeInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NodeInfo {
    pub id: NodeId,
    pub name: String,
    pub connected: bool,
    pub transport: Option<String>,
    pub remote_addr: Option<String>,
    pub rtt_ms: Option<u64>,
}

// --- /bitcoin-connection ---

#[derive(Debug, Serialize, Deserialize)]
pub struct BitcoinConnectionResponse {
    pub network: String,
    pub block_height: u32,
    pub fee_rate_sat_per_vb: Option<u32>,
    pub sync_progress: Option<f64>,
}

// --- /module/onchain/feerate ---

#[derive(Debug, Serialize, Deserialize)]
pub struct OnchainFeerateResponse {
    pub sat_per_vbyte: Option<u32>,
}

// --- /module/onchain/pending-txs ---

#[derive(Debug, Serialize, Deserialize)]
pub struct PendingTxsResponse {
    pub txs: Vec<TxInfo>,
}

// --- /module/onchain/txs ---

#[derive(Debug, Serialize, Deserialize)]
pub struct TxsResponse {
    pub txs: Vec<TxInfo>,
}

// --- /module/onchain/sweep ---

/// Recovery material for sweeping the mint wallet after decommissioning:
/// the aggregate public key and this node's secret key share, both tweaked
/// for the current mint UTXO, as hex. A threshold of nodes' shares
/// interpolates into the UTXO's key. Secret.
#[derive(Debug, Serialize, Deserialize)]
pub struct SweepResponse {
    pub aggregate_public_key: String,
    pub secret_key_share: String,
}

// --- /module/lightning/gateway/* ---

#[derive(Clone, Debug, Serialize, Deserialize, Args)]
pub struct LightningGatewayAddRequest {
    /// Gateway iroh public key (base32-encoded).
    pub pk: picomint_core::lightning::gateway::GatewayPk,
    /// Display name to identify the gateway by.
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Args)]
pub struct LightningGatewayRemoveRequest {
    /// Gateway iroh public key (base32-encoded).
    pub pk: picomint_core::lightning::gateway::GatewayPk,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LightningGatewayListResponse {
    pub gateways: Vec<LightningGatewayInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LightningGatewayInfo {
    /// Gateway iroh public key (base32-encoded).
    pub pk: picomint_core::lightning::gateway::GatewayPk,
    /// Display name to identify the gateway by.
    pub name: String,
}

// --- /expiry/set ---

#[derive(Clone, Debug, Serialize, Deserialize, Args)]
pub struct ExpirySetRequest {
    /// Expiry date as a unix timestamp in seconds (midnight UTC).
    #[arg(long)]
    pub timestamp: u64,
    /// Optional successor-mint invite code (base32-encoded).
    #[arg(long)]
    pub successor: Option<InviteCode>,
}
