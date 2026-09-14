//! Routes and request/response types of the client daemon's admin CLI:
//! mint management, ecash, onchain, lightning, the analytics query and
//! the mnemonic. Every response type carries a JSON Schema, printed by the
//! CLI under `--schema`, so its doc lines are the operator-facing
//! description of each field. The gateway exposes a subset of the same
//! operations over its own admin socket and deliberately keeps its own
//! copies of these types — the two surfaces are allowed to drift, and the
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
use picomint_core::expiry::ExpiryStatus;
use picomint_core::invite::InviteCode;
use picomint_core::lightning::gateway::{GatewayInfo, GatewayPk};
use picomint_lnurl::Lnurl;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

pub const ROUTE_MNEMONIC: &str = "/mnemonic";
pub const ROUTE_QUERY: &str = "/query";

pub const ROUTE_ADD: &str = "/add";
pub const ROUTE_REMOVE: &str = "/remove";
pub const ROUTE_LIST: &str = "/list";
pub const ROUTE_CONFIG: &str = "/config";
pub const ROUTE_EXPIRY: &str = "/expiry";
pub const ROUTE_BALANCE: &str = "/balance";

pub const ROUTE_ECASH_COUNT: &str = "/ecash/count";
pub const ROUTE_ECASH_SEND: &str = "/ecash/send";
pub const ROUTE_ECASH_SEND_MAX: &str = "/ecash/send-max";
pub const ROUTE_ECASH_RECEIVE: &str = "/ecash/receive";
pub const ROUTE_ONCHAIN_SEND_FEE: &str = "/onchain/send-fee";
pub const ROUTE_ONCHAIN_SEND: &str = "/onchain/send";
pub const ROUTE_ONCHAIN_SEND_MAX_AMOUNT: &str = "/onchain/send-max-amount";
pub const ROUTE_ONCHAIN_SEND_MAX: &str = "/onchain/send-max";
pub const ROUTE_ONCHAIN_RECEIVE: &str = "/onchain/receive";

pub const ROUTE_LIGHTNING_GATEWAY_LIST: &str = "/lightning/gateway/list";
pub const ROUTE_LIGHTNING_SEND: &str = "/lightning/send";
pub const ROUTE_LIGHTNING_SEND_MAX_AMOUNT: &str = "/lightning/send-max-amount";
pub const ROUTE_LIGHTNING_SEND_MAX: &str = "/lightning/send-max";
pub const ROUTE_LIGHTNING_RECEIVE: &str = "/lightning/receive";
pub const ROUTE_LIGHTNING_LNURL: &str = "/lightning/lnurl";
pub const ROUTE_LIGHTNING_GATEWAY_REFRESH: &str = "/lightning/gateway/refresh";

// --- /mnemonic ---

/// The seed. Secret: pipe it into a file, never to a terminal.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct MnemonicResponse {
    /// The twelve BIP39 words every mint balance derives from; the app
    /// restores them from these words, this daemon cannot be seeded
    pub mnemonic: Vec<String>,
}

// --- /query ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct QueryRequest {
    /// Read-only SQL run against the analytics db, e.g.
    /// "SELECT * FROM core_tx_accept ORDER BY ts DESC LIMIT 10"
    pub query: String,
}

/// The rows the query returned: one JSON object per row, keyed by result
/// column name, the same shape `sqlite3 --json` prints. Column types
/// follow the analytics schema: amounts as integers, msat except in the
/// onchain tables, which are sat; hashes, ids and keys as text.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct QueryResponse(pub Vec<serde_json::Map<String, serde_json::Value>>);

// --- /add ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientAddRequest {
    /// The mint's invite code, as printed by a node's `invite`; the mint has
    /// to run on this daemon's `NETWORK`
    pub invite: InviteCode,
}

// --- /remove ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientRemoveRequest {
    /// The mint id, as printed by `list`
    pub mint: MintId,
}

// --- /expiry ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientExpiryRequest {
    /// The mint id, as printed by `list`
    pub mint: MintId,
}

/// The mint's expiry announcement, fetched fresh from its nodes.
#[derive(Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct ClientExpiryResponse {
    /// The date the mint winds down, as midnight UTC in unix seconds, and
    /// the successor mint to move funds to, once a threshold of nodes
    /// announce the same values; absent while they announce nothing
    pub expiry: Option<ExpiryStatus>,
}

// --- /balance ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientBalanceRequest {
    /// The mint id, as printed by `list`
    pub mint: MintId,
    /// One of `primary`, `secondary`, `tertiary`, `quaternary`, `quinary`:
    /// five independent balances under one seed, told apart by nothing but
    /// their derivation path
    pub account: Account,
}

/// An account's ecash balance in one mint.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientBalanceResponse {
    /// The sum of the notes the account holds, in msat
    pub balance_msat: Amount,
}

// --- /list ---

/// The mints the daemon has added.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientListResponse {
    /// The mints, ordered by id
    pub mints: Vec<MintInfo>,
}

/// One added mint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct MintInfo {
    /// The mint id, which every other command takes first
    pub mint: MintId,
    /// The mint's name from its config
    pub mint_name: String,
}

// --- /config ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientConfigRequest {
    /// The mint id, as printed by `list`
    pub mint: MintId,
}

/// The mint's consensus config as its nodes serve it.
#[derive(Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct ClientConfigResponse {
    /// The config: name, network, node set with iroh keys, module public
    /// keys and fee parameters. Its shape is the mint's, not this CLI's
    pub config: serde_json::Value,
}

// --- /ecash/count ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientEcashCountRequest {
    /// The mint id, as printed by `list`
    pub mint: MintId,
    /// The account, as for `balance`
    pub account: Account,
}

/// The account's balance broken down into the notes that make it up, which
/// is what a load test watches to see the note pool it draws from.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientEcashCountResponse {
    /// Notes held per denomination, keyed by the denomination's exponent as
    /// a string: key `"10"` counts the notes worth 2^10 msat
    pub counts: BTreeMap<Denomination, u64>,
}

// --- /ecash/send ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientEcashSendRequest {
    /// The mint id, as printed by `list`
    pub mint: MintId,
    /// The account, as for `balance`
    pub account: Account,
    /// The amount with its denomination, e.g. "1000 sat"; rounded up to a
    /// multiple of the smallest note, and reissued first when the notes on
    /// hand cannot make it up exactly
    pub amount: bitcoin::Amount,
}

/// The bundle to hand over.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientEcashSendResponse {
    /// The ecash; its notes have left the account's balance and belong to
    /// whoever receives the string first
    pub ecash: Ecash,
}

// --- /ecash/send-max ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientEcashSendMaxRequest {
    /// The mint id, as printed by `list`
    pub mint: MintId,
    /// The account, as for `balance`
    pub account: Account,
}

/// The account's whole balance as one bundle.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientEcashSendMaxResponse {
    /// The ecash, every note the account held; absent when it held none
    pub ecash: Option<Ecash>,
}

// --- /ecash/receive ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientEcashReceiveRequest {
    /// The mint id, as printed by `list`
    pub mint: MintId,
    /// The account, as for `balance`
    pub account: Account,
    /// A bundle from any client's `ecash send`; each bundle can be received
    /// once per mint
    pub ecash: Ecash,
}

/// The reissue was submitted; it completes in the background.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientEcashReceiveResponse {
    /// The operation the reissue logs under, derived from the bundle;
    /// acceptance shows up in the analytics as `core_tx_accept`
    pub operation: OperationId,
}

// --- /onchain/send-fee ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientOnchainSendFeeRequest {
    /// The mint id, as printed by `list`
    pub mint: MintId,
}

/// What a send costs right now.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientOnchainSendFeeResponse {
    /// The miner fee the mint requires for one send transaction, in sat:
    /// the consensus fee rate times the 154 vbytes of a send, raised while
    /// a stack of pending mint transactions has to be paid for. The mint's
    /// own per-output fee from its config comes on top of this when the
    /// send is charged
    #[schemars(with = "u64")]
    pub fee: bitcoin::Amount,
}

// --- /onchain/send ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientOnchainSendRequest {
    /// The mint id, as printed by `list`
    pub mint: MintId,
    /// The account, as for `balance`
    pub account: Account,
    /// The destination address, on the mint's network
    pub address: bitcoin::Address<NetworkUnchecked>,
    /// The amount with its denomination, e.g. "100000 sat"; at least the
    /// mint's dust limit
    pub amount: bitcoin::Amount,
    /// A miner fee to attach instead of the one `send-fee` quotes; the mint
    /// rejects the send if this is below what it requires at the time, so
    /// only ever raise it
    #[arg(long)]
    pub fee: Option<bitcoin::Amount>,
}

/// The send was submitted; it completes in the background.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientOnchainSendResponse {
    /// The operation the send logs under; `onchain_send_success` carries
    /// the txid once the mint has broadcast
    pub operation: OperationId,
}

// --- /onchain/send-max-amount ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientOnchainSendMaxAmountRequest {
    /// The mint id, as printed by `list`
    pub mint: MintId,
    /// The account, as for `balance`
    pub account: Account,
}

/// What `onchain send-max` would move right now.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientOnchainSendMaxAmountResponse {
    /// The amount the destination receives, in sat: the largest whole-sat
    /// amount the account's notes cover when spent in full, once the miner
    /// fee `send-fee` quotes, the mint's per-output fee and its per-input
    /// fee on every note are paid; the sub-sat remainder stays with the
    /// mint. 0 when the notes do not even cover the fees
    #[schemars(with = "u64")]
    pub amount_sat: bitcoin::Amount,
}

// --- /onchain/send-max ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientOnchainSendMaxRequest {
    /// The mint id, as printed by `list`
    pub mint: MintId,
    /// The account, as for `balance`
    pub account: Account,
    /// The destination address, on the mint's network
    pub address: bitcoin::Address<NetworkUnchecked>,
}

/// The send was submitted; it completes in the background.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientOnchainSendMaxResponse {
    /// The operation the send logs under; `onchain_send_success` carries
    /// the txid once the mint has broadcast
    pub operation: OperationId,
}

// --- /onchain/receive ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientOnchainReceiveRequest {
    /// The mint id, as printed by `list`
    pub mint: MintId,
    /// The account, as for `balance`
    pub account: Account,
}

/// Where to send bitcoin to have the mint issue ecash for it.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientOnchainReceiveResponse {
    /// The account's next unused deposit address, a taproot address of the
    /// mint's wallet key. A deposit is credited as ecash once a threshold
    /// of nodes see it 6 confirmations deep, less the miner fee of the
    /// transaction that sweeps it into the mint wallet and the mint's
    /// per-input fee
    #[schemars(with = "String")]
    pub address: bitcoin::Address<NetworkUnchecked>,
}

// --- /lightning/gateway/list ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientLightningGatewayListRequest {
    /// The mint id, as printed by `list`
    pub mint: MintId,
}

/// Every gateway the mint recommends that answered a probe, keyed by pk,
/// with the fees it charges. The list only changes on `lightning gateway
/// refresh`, so a fee read here is the fee a send between refreshes pays.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientLightningGatewayListResponse {
    /// The gateways, keyed by `gateway_pk`; every send and receive names
    /// one of these keys
    pub gateways: BTreeMap<GatewayPk, GatewayInfo>,
}

// --- /lightning/send ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientLightningSendRequest {
    /// The mint id, as printed by `list`
    pub mint: MintId,
    /// The account, as for `balance`
    pub account: Account,
    /// The gateway to pay through, from `lightning gateway list`
    pub gateway: GatewayPk,
    /// The bolt11 invoice to pay; the gateway's `send_fee` is charged on
    /// top of its amount
    pub invoice: Bolt11Invoice,
}

/// The payment was submitted; it completes in the background.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientLightningSendResponse {
    /// The operation the payment logs under, derived from the invoice's
    /// payment hash; the outcome is `lightning_send_success` with the
    /// preimage, or `lightning_send_refund` if the gateway could not route
    pub operation: OperationId,
}

// --- /lightning/send-max-amount ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientLightningSendMaxAmountRequest {
    /// The mint id, as printed by `list`
    pub mint: MintId,
    /// The account, as for `balance`
    pub account: Account,
    /// The gateway to pay through, from `lightning gateway list`
    pub gateway: GatewayPk,
}

/// What `lightning send-max` would pay right now.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientLightningSendMaxAmountResponse {
    /// The invoice amount, in msat and always a whole sat: the largest
    /// amount the account's notes cover when spent in full, once the
    /// gateway's `send_fee` on it, the mint's per-output fee and its
    /// per-input fee on every note are paid; the sub-sat remainder stays
    /// with the mint. 0 when the notes do not even cover the fees
    pub amount_msat: Amount,
}

// --- /lightning/send-max ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientLightningSendMaxRequest {
    /// The mint id, as printed by `list`
    pub mint: MintId,
    /// The account, as for `balance`
    pub account: Account,
    /// The gateway to pay through, from `lightning gateway list`
    pub gateway: GatewayPk,
    /// The lnurl or lightning address to pay; the account's whole balance
    /// less the gateway's fee goes to it
    pub lnurl: Lnurl,
}

/// The payment was submitted; it completes in the background.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientLightningSendMaxResponse {
    /// The operation the payment logs under, derived from the invoice's
    /// payment hash; the outcome is `lightning_send_success` with the
    /// preimage, or `lightning_send_refund` if the gateway could not route
    pub operation: OperationId,
}

// --- /lightning/receive ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientLightningReceiveRequest {
    /// The mint id, as printed by `list`
    pub mint: MintId,
    /// The account, as for `balance`
    pub account: Account,
    /// The gateway to receive through, from `lightning gateway list`
    pub gateway: GatewayPk,
    /// The amount with its denomination the payer pays, e.g. "1000 sat";
    /// the gateway's `receive_fee` comes out of it
    pub amount: bitcoin::Amount,
}

/// An invoice for the account, issued by the gateway.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientLightningReceiveResponse {
    /// The bolt11 invoice; the payment lands in the analytics as
    /// `lightning_receive` under the operation derived from its payment
    /// hash, once the gateway has funded it
    #[schemars(with = "String")]
    pub invoice: Bolt11Invoice,
}

// --- /lightning/lnurl ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientLightningLnurlRequest {
    /// The mint id, as printed by `list`
    pub mint: MintId,
    /// The account, as for `balance`
    pub account: Account,
    /// Base URL of the lnurl daemon that serves the lnurl, e.g.
    /// `https://lnurl.example.com/`
    pub lnurl_daemon: Url,
}

/// A reusable way to be paid while this daemon is offline.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientLightningLnurlResponse {
    /// The bech32 lnurl; valid for as long as the mint exists, since its
    /// payload carries nothing that expires
    pub lnurl: String,
}

// --- /lightning/gateway/refresh ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientLightningGatewayRefreshRequest {
    /// The mint id, as printed by `list`
    pub mint: MintId,
}
