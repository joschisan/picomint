use clap::Args;
use picomint_core::PeerId;
use picomint_core::invite::InviteCode;
use picomint_core::module::audit::AuditSummary;
use picomint_core::wallet::TxInfo;
use serde::{Deserialize, Serialize};

/// Filename of the guardian's admin CLI Unix socket, inside `DATA_DIR`.
/// The daemon binds and the CLI connects at `{DATA_DIR}/{CLI_SOCKET_FILENAME}`.
pub const CLI_SOCKET_FILENAME: &str = "cli.sock";

/// Status of the setup flow — mirrors the guardian UI's `SetupStatus`
/// as a CLI-consumed copy so `picomint-guardian-cli` doesn't need to pull in
/// the daemon crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SetupStatus {
    AwaitingLocalParams,
    SharingConnectionCodes,
}

// Setup routes
pub const ROUTE_SETUP_STATUS: &str = "/setup/status";
pub const ROUTE_SETUP_SET_LOCAL_PARAMS: &str = "/setup/set-local-params";
pub const ROUTE_SETUP_ADD_PEER: &str = "/setup/add-peer";
pub const ROUTE_SETUP_START_DKG: &str = "/setup/start-dkg";
pub const ROUTE_SETUP_RESTORE: &str = "/setup/restore";

// Dashboard routes
pub const ROUTE_INVITE: &str = "/invite";
pub const ROUTE_AUDIT: &str = "/audit";
pub const ROUTE_CONFIG: &str = "/config";
pub const ROUTE_SESSION_COUNT: &str = "/session-count";
pub const ROUTE_BLOCK_COUNT: &str = "/block-count";
pub const ROUTE_P2P: &str = "/p2p";
pub const ROUTE_BITCOIN_CONNECTION: &str = "/bitcoin-connection";
pub const ROUTE_EXPIRY_SET: &str = "/expiry/set";
pub const ROUTE_EXPIRY_CLEAR: &str = "/expiry/clear";
pub const ROUTE_EXPIRY_STATUS: &str = "/expiry/status";

// Module routes
pub const ROUTE_MODULE_WALLET_TOTAL_VALUE: &str = "/module/wallet/total-value";
pub const ROUTE_MODULE_WALLET_FEERATE: &str = "/module/wallet/feerate";
pub const ROUTE_MODULE_WALLET_PENDING_TXS: &str = "/module/wallet/pending-txs";
pub const ROUTE_MODULE_WALLET_TXS: &str = "/module/wallet/txs";
pub const ROUTE_MODULE_LN_GATEWAY_ADD: &str = "/module/ln/gateway/add";
pub const ROUTE_MODULE_LN_GATEWAY_REMOVE: &str = "/module/ln/gateway/remove";
pub const ROUTE_MODULE_LN_GATEWAY_LIST: &str = "/module/ln/gateway/list";

// --- /setup/status ---
// Response: SetupStatus (defined above)

// --- /setup/set-local-params ---

#[derive(Clone, Debug, Serialize, Deserialize, Args)]
pub struct SetupSetLocalParamsRequest {
    /// Guardian name
    pub name: String,
    /// Federation name (leader only)
    #[arg(long)]
    pub federation_name: Option<String>,
    /// Federation size (leader only)
    #[arg(long)]
    pub federation_size: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SetupSetLocalParamsResponse {
    pub setup_code: String,
}

// --- /setup/add-peer ---

#[derive(Clone, Debug, Serialize, Deserialize, Args)]
pub struct SetupAddPeerRequest {
    /// Peer's setup code
    pub setup_code: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SetupAddPeerResponse {
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
pub const DEFAULT_INVITE_USER_LIMIT: u64 = 50;

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct InviteRequest {
    /// Days until the invite code expires.
    #[arg(long = "days", default_value_t = DEFAULT_INVITE_EXPIRY_DAYS)]
    pub expiry_days: u64,
    /// Maximum number of users that may onboard with this invite code.
    #[arg(long = "users", default_value_t = DEFAULT_INVITE_USER_LIMIT)]
    pub user_limit: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InviteResponse {
    pub invite: InviteCode,
}

// --- /audit ---

#[derive(Debug, Serialize, Deserialize)]
pub struct AuditResponse {
    pub audit: AuditSummary,
}

// --- /module/wallet/total-value ---

#[derive(Debug, Serialize, Deserialize)]
pub struct WalletTotalValueResponse {
    pub total_value_sat: Option<u64>,
}

// --- /block-count ---

#[derive(Debug, Serialize, Deserialize)]
pub struct BlockCountResponse {
    pub block_count: u32,
}

// --- /p2p ---

#[derive(Debug, Serialize, Deserialize)]
pub struct P2pResponse {
    pub peers: Vec<PeerInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PeerInfo {
    pub id: PeerId,
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
    pub block_count: u32,
    pub fee_rate_sat_per_vb: Option<u64>,
    pub sync_progress: Option<f64>,
}

// --- /module/wallet/feerate ---

#[derive(Debug, Serialize, Deserialize)]
pub struct WalletFeerateResponse {
    pub sat_per_vbyte: Option<u64>,
}

// --- /module/wallet/pending-txs ---

#[derive(Debug, Serialize, Deserialize)]
pub struct PendingTxsResponse {
    pub txs: Vec<TxInfo>,
}

// --- /module/wallet/txs ---

#[derive(Debug, Serialize, Deserialize)]
pub struct TxsResponse {
    pub txs: Vec<TxInfo>,
}

// --- /module/ln/gateway/* ---

#[derive(Clone, Debug, Serialize, Deserialize, Args)]
pub struct LnGatewayAddRequest {
    /// Gateway iroh public key (base32-encoded).
    pub pk: picomint_core::ln::gateway::GatewayPk,
    /// Display name to identify the gateway by.
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Args)]
pub struct LnGatewayRemoveRequest {
    /// Gateway iroh public key (base32-encoded).
    pub pk: picomint_core::ln::gateway::GatewayPk,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LnGatewayListResponse {
    pub gateways: Vec<LnGatewayInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LnGatewayInfo {
    /// Gateway iroh public key (base32-encoded).
    pub pk: picomint_core::ln::gateway::GatewayPk,
    /// Display name to identify the gateway by.
    pub name: String,
}

// --- /expiry/set ---

#[derive(Clone, Debug, Serialize, Deserialize, Args)]
pub struct ExpirySetRequest {
    /// Expiry date as a unix timestamp in seconds (midnight UTC).
    #[arg(long)]
    pub timestamp: u64,
    /// Optional successor-federation invite code (base32-encoded).
    #[arg(long)]
    pub successor: Option<InviteCode>,
}
