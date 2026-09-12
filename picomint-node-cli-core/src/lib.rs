use clap::Args;
use picomint_core::NodeId;
use picomint_core::bitcoin::Txid;
use picomint_core::config::MintId;
use picomint_core::invite::InviteCode;
use picomint_core::onchain::TxInfo;
use picomint_core::version::ConsensusVersion;
use serde::{Deserialize, Serialize};

/// Served in every phase of the node's life; everything else is phase-bound.
pub const ROUTE_STATUS: &str = "/status";

// Setup routes
pub const ROUTE_SETUP_INIT: &str = "/setup/init";
pub const ROUTE_SETUP_ADD: &str = "/setup/add";
pub const ROUTE_SETUP_RESET: &str = "/setup/reset";
pub const ROUTE_SETUP_CONFIRM: &str = "/setup/confirm";
pub const ROUTE_SETUP_RESTORE: &str = "/setup/restore";

// Consensus routes
pub const ROUTE_INVITE: &str = "/invite";
pub const ROUTE_BACKUP: &str = "/backup";
pub const ROUTE_EXPIRY_SET: &str = "/expiry/set";
pub const ROUTE_EXPIRY_CLEAR: &str = "/expiry/clear";
pub const ROUTE_EXPIRY_STATUS: &str = "/expiry/status";
pub const ROUTE_QUERY: &str = "/query";

// Module routes
pub const ROUTE_ONCHAIN_STATUS: &str = "/onchain/status";
pub const ROUTE_ONCHAIN_PENDING: &str = "/onchain/pending";
pub const ROUTE_ONCHAIN_HISTORY: &str = "/onchain/history";
pub const ROUTE_ONCHAIN_SWEEP: &str = "/onchain/sweep";
pub const ROUTE_GATEWAY_ADD: &str = "/gateway/add";
pub const ROUTE_GATEWAY_REMOVE: &str = "/gateway/remove";
pub const ROUTE_GATEWAY_LIST: &str = "/gateway/list";

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
/// private keys are only ever returned by `backup`.
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
    pub nodes: Vec<NodeInfo>,
    pub bitcoin: Option<BitcoinConnectionResponse>,
}

// --- /setup/init ---

#[derive(Clone, Debug, Serialize, Deserialize, Args)]
pub struct SetupInitRequest {
    /// This node's name, shown to the other nodes and to clients
    pub name: String,
    /// The mint's name; set by exactly one node
    #[arg(long)]
    pub mint_name: Option<String>,
    /// Number of nodes in the mint: 4, 7, 10, 13, 16, 19 or 22; set by the same node
    #[arg(long)]
    pub mint_size: Option<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SetupInitResponse {
    pub setup_code: String,
}

// --- /setup/add ---

#[derive(Clone, Debug, Serialize, Deserialize, Args)]
pub struct SetupAddRequest {
    /// Another node's setup code, as printed by its `setup init`
    pub setup_code: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SetupAddResponse {
    pub name: String,
}

// --- /setup/confirm ---
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

// --- /onchain/status ---

/// The mint wallet at a glance. `tx_tip` is `None` until the first deposit
/// has established the wallet; `onchain pending` lists what is in flight.
#[derive(Debug, Serialize, Deserialize)]
pub struct OnchainStatusResponse {
    pub total_value_sat: u64,
    /// The transaction holding the mint's current wallet UTXO.
    pub tx_tip: Option<Txid>,
    pub tx_count: u64,
    pub feerate_sat_per_vb: Option<u32>,
}

// --- status: nodes ---

#[derive(Debug, Serialize, Deserialize)]
pub struct NodeInfo {
    pub id: NodeId,
    pub name: String,
    pub connected: bool,
    pub transport: Option<String>,
    pub remote_addr: Option<String>,
    pub rtt_ms: Option<u64>,
}

// --- status: bitcoin backend ---

#[derive(Debug, Serialize, Deserialize)]
pub struct BitcoinConnectionResponse {
    pub network: String,
    pub block_height: u32,
    pub fee_rate_sat_per_vb: Option<u32>,
    pub sync_progress: Option<f64>,
}

// --- /onchain/pending ---

#[derive(Debug, Serialize, Deserialize)]
pub struct PendingResponse {
    pub txs: Vec<TxInfo>,
}

// --- /onchain/history ---

#[derive(Debug, Serialize, Deserialize)]
pub struct HistoryResponse {
    pub txs: Vec<TxInfo>,
}

// --- /onchain/sweep ---

/// This node's base32 [`picomint_core::onchain::SweepSecret`] for the current
/// mint UTXO. A threshold of nodes' secrets sweeps the wallet after
/// decommissioning. Secret.
#[derive(Debug, Serialize, Deserialize)]
pub struct SweepResponse {
    pub secret: String,
}

// --- /gateway/* ---

#[derive(Clone, Debug, Serialize, Deserialize, Args)]
pub struct LightningGatewayAddRequest {
    /// The gateway's `gateway_pk`, as printed by `picomint-gateway-cli info`
    pub pk: picomint_core::lightning::gateway::GatewayPk,
    /// Display name to identify the gateway by
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Args)]
pub struct LightningGatewayRemoveRequest {
    /// The gateway's `gateway_pk`, as printed by `picomint-gateway-cli info`
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
    /// Expiry date as a unix timestamp in seconds, midnight UTC
    #[arg(long)]
    pub timestamp: u64,
    /// Invite code of the successor mint for users to migrate to
    #[arg(long)]
    pub successor: Option<InviteCode>,
}

// --- /query ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct QueryRequest {
    /// Read-only SQL run against the analytics db, e.g.
    /// "SELECT * FROM tx ORDER BY session DESC, idx DESC LIMIT 10"
    pub query: String,
}

/// One JSON object per row, keyed by result column name — the same shape
/// `sqlite3 --json` prints.
pub type QueryResponse = Vec<serde_json::Map<String, serde_json::Value>>;
