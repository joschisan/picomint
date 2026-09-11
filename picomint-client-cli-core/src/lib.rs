//! Routes and request/response types of the client daemon's admin CLI:
//! mint management, ecash, onchain, lightning, the analytics query and
//! the mnemonic. The gateway exposes a subset of the same operations
//! over its own admin socket and deliberately keeps its own copies of
//! these types — the two surfaces are allowed to drift, and the
//! compiler polices each against the client library on its own.

use std::collections::BTreeMap;

use bitcoin::address::NetworkUnchecked;
use clap::Args;
use lightning_invoice::Bolt11Invoice;
use picomint_client::ecash::Ecash;
use picomint_core::Amount;
use picomint_core::config::MintId;
use picomint_core::core::{Account, OperationId};
use picomint_core::ecash::Denomination;
use picomint_core::invite::InviteCode;
use serde::{Deserialize, Serialize};

pub const ROUTE_MNEMONIC: &str = "/mnemonic";
pub const ROUTE_QUERY: &str = "/query";

pub const ROUTE_ADD: &str = "/add";
pub const ROUTE_REMOVE: &str = "/remove";
pub const ROUTE_LIST: &str = "/list";
pub const ROUTE_CONFIG: &str = "/config";
pub const ROUTE_BALANCE: &str = "/balance";

pub const ROUTE_ECASH_COUNT: &str = "/ecash/count";
pub const ROUTE_ECASH_SEND: &str = "/ecash/send";
pub const ROUTE_ECASH_SEND_MAX: &str = "/ecash/send-max";
pub const ROUTE_ECASH_RECEIVE: &str = "/ecash/receive";
pub const ROUTE_ONCHAIN_SEND_FEE: &str = "/onchain/send-fee";
pub const ROUTE_ONCHAIN_SEND: &str = "/onchain/send";
pub const ROUTE_ONCHAIN_SEND_MAX: &str = "/onchain/send-max";
pub const ROUTE_ONCHAIN_RECEIVE: &str = "/onchain/receive";

pub const ROUTE_LIGHTNING_SEND: &str = "/lightning/send";
pub const ROUTE_LIGHTNING_SEND_MAX: &str = "/lightning/send-max";
pub const ROUTE_LIGHTNING_RECEIVE: &str = "/lightning/receive";
pub const ROUTE_LIGHTNING_LNURL: &str = "/lightning/lnurl";
pub const ROUTE_LIGHTNING_REFRESH_GATEWAYS: &str = "/lightning/refresh-gateways";

// --- /mnemonic ---

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MnemonicResponse {
    pub mnemonic: Vec<String>,
}

// --- /query ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct QueryRequest {
    /// Read-only SQL run against the analytics db, e.g.
    /// "SELECT * FROM core_tx_accept ORDER BY ts DESC LIMIT 10"
    pub query: String,
}

/// One JSON object per row, keyed by result column name — the same shape
/// `sqlite3 --json` prints.
pub type QueryResponse = Vec<serde_json::Map<String, serde_json::Value>>;

// --- /add ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientAddRequest {
    pub invite: InviteCode,
}

// --- /remove ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientRemoveRequest {
    pub mint: MintId,
}

// --- /balance ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientBalanceRequest {
    pub mint: MintId,
    pub account: Account,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientBalanceResponse {
    pub balance_msat: Amount,
}

// --- /list ---

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientListResponse {
    pub mints: Vec<MintInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MintInfo {
    pub mint: MintId,
    pub mint_name: String,
}

// --- /config ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientConfigRequest {
    pub mint: MintId,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct ClientConfigResponse {
    pub config: serde_json::Value,
}

// --- /ecash/count ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientEcashCountRequest {
    pub mint: MintId,
    pub account: Account,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientEcashCountResponse {
    /// Count of held ecash notes keyed by denomination.
    pub counts: BTreeMap<Denomination, u64>,
}

// --- /ecash/send ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientEcashSendRequest {
    pub mint: MintId,
    pub account: Account,
    pub amount: bitcoin::Amount,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientEcashSendResponse {
    pub ecash: Ecash,
}

// --- /ecash/send-max ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientEcashSendMaxRequest {
    pub mint: MintId,
    pub account: Account,
}

/// `None` when the account holds no notes.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientEcashSendMaxResponse {
    pub ecash: Option<Ecash>,
}

// --- /ecash/receive ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientEcashReceiveRequest {
    pub mint: MintId,
    pub account: Account,
    pub ecash: Ecash,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientEcashReceiveResponse {
    pub operation: OperationId,
}

// --- /onchain/send-fee ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientOnchainSendFeeRequest {
    pub mint: MintId,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientOnchainSendFeeResponse {
    pub fee: bitcoin::Amount,
}

// --- /onchain/send ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientOnchainSendRequest {
    pub mint: MintId,
    pub account: Account,
    pub address: bitcoin::Address<NetworkUnchecked>,
    pub amount: bitcoin::Amount,
    #[arg(long)]
    pub fee: Option<bitcoin::Amount>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientOnchainSendResponse {
    pub operation: OperationId,
}

// --- /onchain/send-max ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientOnchainSendMaxRequest {
    pub mint: MintId,
    pub account: Account,
    pub address: bitcoin::Address<NetworkUnchecked>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientOnchainSendMaxResponse {
    pub operation: OperationId,
}

// --- /onchain/receive ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientOnchainReceiveRequest {
    pub mint: MintId,
    pub account: Account,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientOnchainReceiveResponse {
    pub address: bitcoin::Address<NetworkUnchecked>,
}

// --- /lightning/send ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientLightningSendRequest {
    pub mint: MintId,
    pub account: Account,
    pub invoice: Bolt11Invoice,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientLightningSendResponse {
    pub operation: OperationId,
}

// --- /lightning/send-max ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientLightningSendMaxRequest {
    pub mint: MintId,
    pub account: Account,
    pub lnurl: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientLightningSendMaxResponse {
    pub operation: OperationId,
}

// --- /lightning/receive ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientLightningReceiveRequest {
    pub mint: MintId,
    pub account: Account,
    pub amount: bitcoin::Amount,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientLightningReceiveResponse {
    pub operation: OperationId,
    pub invoice: Bolt11Invoice,
}

// --- /lightning/lnurl ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientLightningLnurlRequest {
    pub mint: MintId,
    pub account: Account,
    /// Base URL of the lnurl daemon that serves the lnurl, e.g.
    /// `https://lnurl.example.com/`
    pub lnurl_daemon: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientLightningLnurlResponse {
    pub lnurl: String,
}

// --- /lightning/refresh-gateways ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientLightningRefreshGatewaysRequest {
    pub mint: MintId,
}
