//! Routes and request/response types of the broker daemon's admin CLI.
//! Every response type carries a JSON Schema, printed by the CLI under
//! `--schema`, so its doc lines are the operator-facing description of each
//! field. The `client` commands mirror the client daemon's and deliberately
//! keep their own copies of the types — the two surfaces are allowed to
//! drift, and the compiler polices each against the client library on its
//! own.

use std::collections::BTreeMap;

use bitcoin::address::NetworkUnchecked;
use clap::Args;
use picomint_client::ecash::Ecash;
use picomint_client::onchain;
use picomint_core::Amount;
use picomint_core::config::MintId;
use picomint_core::core::{Account, OperationId};
use picomint_core::ecash::Denomination;
use picomint_core::error::ErrorCode;
use picomint_core::invite::InviteCode;
use picomint_core::lightning::gateway::PaymentFee;
use picomint_core::swap::broker::BrokerPk;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// Top-level
pub const ROUTE_INFO: &str = "/info";
pub const ROUTE_MNEMONIC: &str = "/mnemonic";
pub const ROUTE_QUERY: &str = "/query";
pub const ROUTE_REBALANCE: &str = "/rebalance";

// Mint management
pub const ROUTE_CLIENT_ADD: &str = "/client/add";
pub const ROUTE_CLIENT_LIST: &str = "/client/list";
pub const ROUTE_CLIENT_CONFIG: &str = "/client/config";
pub const ROUTE_CLIENT_BALANCE: &str = "/client/balance";
pub const ROUTE_CLIENT_REMOVE: &str = "/client/remove";

// Per-mint module commands
pub const ROUTE_CLIENT_ECASH_COUNT: &str = "/client/ecash/count";
pub const ROUTE_CLIENT_ECASH_SEND: &str = "/client/ecash/send";
pub const ROUTE_CLIENT_ECASH_SEND_MAX: &str = "/client/ecash/send-max";
pub const ROUTE_CLIENT_ECASH_RECEIVE: &str = "/client/ecash/receive";
pub const ROUTE_CLIENT_ONCHAIN_SEND_FEE: &str = "/client/onchain/send-fee";
pub const ROUTE_CLIENT_ONCHAIN_RECEIVE_FEE: &str = "/client/onchain/receive-fee";
pub const ROUTE_CLIENT_ONCHAIN_SEND: &str = "/client/onchain/send";
pub const ROUTE_CLIENT_ONCHAIN_SEND_MAX: &str = "/client/onchain/send-max";
pub const ROUTE_CLIENT_ONCHAIN_RECEIVE: &str = "/client/onchain/receive";

// --- /info ---

/// The broker's identity and pricing. The broker is one client of every
/// mint it serves, holding a balance in each to fund swaps into it.
#[derive(Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct InfoResponse {
    /// The broker's identity towards mints and their clients, its iroh
    /// public key. Mint nodes recommend it via `picomint-node-cli broker
    /// add`
    pub broker_pk: BrokerPk,
    /// The Bitcoin network from `NETWORK`; every mint the broker serves has
    /// to run on it
    pub network: String,
    /// The fee the broker charges on every swap, on top of the amount the
    /// recipient receives
    pub fee: PaymentFee,
}

// --- /mnemonic ---

/// The seed. Secret: pipe it into a file, never to a terminal.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct MnemonicResponse {
    /// The twelve BIP39 words every mint balance derives from
    pub mnemonic: Vec<String>,
}

// --- /query ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct QueryRequest {
    /// Read-only SQL run against the analytics db, e.g.
    /// "SELECT * FROM swap_broker_fund ORDER BY ts DESC LIMIT 10"
    pub query: String,
}

/// The rows the query returned: one JSON object per row, keyed by result
/// column name, the same shape `sqlite3 --json` prints. Column types
/// follow the analytics schema: amounts as integers, msat except in the
/// onchain tables, which are sat; hashes, ids and keys as text.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct QueryResponse(pub Vec<serde_json::Map<String, serde_json::Value>>);

// --- /client/add ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientAddRequest {
    /// The mint's invite code, as printed by a node's `invite`; the mint has
    /// to run on this broker's `NETWORK`
    pub invite: InviteCode,
}

/// The mint is added.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientAddResponse {
    /// The mint's id, which every per-mint command takes first
    pub mint: MintId,
}

// --- /client/remove ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientRemoveRequest {
    /// The mint id, as printed by `client list`
    pub mint: MintId,
}

// --- /client/balance ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientBalanceRequest {
    /// The mint id, as printed by `client list`
    pub mint: MintId,
    /// One of `primary`, `secondary`, `tertiary`, `quaternary`, `quinary`;
    /// swaps are funded from `primary`, the others are reserves
    pub account: Account,
}

/// An account's ecash balance in one mint.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientBalanceResponse {
    /// The sum of the notes the account holds, in msat
    pub balance_msat: Amount,
}

// --- /client/list ---

/// The mints the broker has added, whether or not their nodes recommend
/// it yet.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientListResponse {
    /// The mints, ordered by id
    pub mints: Vec<MintInfo>,
}

/// One added mint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct MintInfo {
    /// The mint id, which every per-mint command takes first
    pub mint: MintId,
    /// The mint's name from its config
    pub mint_name: String,
}

// --- /client/config ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientConfigRequest {
    /// The mint id, as printed by `client list`
    pub mint: MintId,
}

/// The mint's consensus config as its nodes serve it.
#[derive(Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct ClientConfigResponse {
    /// The config: name, network, node set with iroh keys, module public
    /// keys and fee parameters. Its shape is the mint's, not this CLI's
    pub config: serde_json::Value,
}

// --- /client/ecash/count ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientEcashCountRequest {
    /// The mint id, as printed by `client list`
    pub mint: MintId,
    /// The account, as for `client balance`
    pub account: Account,
}

/// The account's balance broken down into the notes that make it up.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientEcashCountResponse {
    /// Notes held per denomination, keyed by the denomination's exponent as
    /// a string: key `"10"` counts the notes worth 2^10 msat
    pub counts: BTreeMap<Denomination, u64>,
}

// --- /client/ecash/send ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientEcashSendRequest {
    /// The mint id, as printed by `client list`
    pub mint: MintId,
    /// The account, as for `client balance`
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

// --- /client/ecash/send-max ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientEcashSendMaxRequest {
    /// The mint id, as printed by `client list`
    pub mint: MintId,
    /// The account, as for `client balance`
    pub account: Account,
}

/// The account's whole balance as one bundle.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientEcashSendMaxResponse {
    /// The ecash, every note the account held; absent when it held none
    pub ecash: Option<Ecash>,
}

// --- /client/ecash/receive ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientEcashReceiveRequest {
    /// The mint id, as printed by `client list`
    pub mint: MintId,
    /// The account, as for `client balance`
    pub account: Account,
    /// A bundle from any client's `ecash send`; each bundle can be received
    /// once per mint
    pub ecash: Ecash,
}

/// The reissue was submitted; it completes in the background.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientEcashReceiveResponse {
    /// The operation the reissue logs under, derived from the bundle;
    /// acceptance shows up in the analytics as `tx_accept`
    pub operation: OperationId,
}

// --- /client/onchain/send-fee ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientOnchainSendFeeRequest {
    /// The mint id, as printed by `client list`
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

// --- /client/onchain/receive-fee ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientOnchainReceiveFeeRequest {
    /// The mint id, as printed by `client list`
    pub mint: MintId,
}

/// What a deposit is credited less right now.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientOnchainReceiveFeeResponse {
    /// The miner fee the mint takes out of a deposit to sweep it into its
    /// wallet, in sat: the consensus fee rate times the vbytes of the
    /// sweep. A deposit worth no more than this is not claimed
    #[schemars(with = "u64")]
    pub fee: bitcoin::Amount,
}

// --- /client/onchain/send ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientOnchainSendRequest {
    /// The mint id, as printed by `client list`
    pub mint: MintId,
    /// The account, as for `client balance`
    pub account: Account,
    /// The destination address, on the mint's network
    pub address: bitcoin::Address<NetworkUnchecked>,
    /// The amount with its denomination, e.g. "100000 sat"; at least the
    /// mint's dust limit
    pub amount: bitcoin::Amount,
    /// A miner fee with its denomination, e.g. "154 sat", to attach instead
    /// of the one `send-fee` quotes; the mint rejects the send if this is
    /// below what it requires at the time, so only ever raise it
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

// --- /client/onchain/send-max ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientOnchainSendMaxRequest {
    /// The mint id, as printed by `client list`
    pub mint: MintId,
    /// The account, as for `client balance`
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

// --- /client/onchain/receive ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientOnchainReceiveRequest {
    /// The mint id, as printed by `client list`
    pub mint: MintId,
    /// The account, as for `client balance`
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
    pub address: bitcoin::Address<bitcoin::address::NetworkUnchecked>,
}

// --- /rebalance ---

/// The onchain transfer made from the mint with the highest primary
/// balance to the one with the lowest, by the smaller of the source's
/// surplus and the destination's deficit against the mean of every
/// mint's balance, so one of the two lands on the mean. Only made when the
/// miner fees on both sides fit the broker's own fee on that amount;
/// `null` when they do not, in which case nothing moved.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct RebalanceResponse {
    /// The mint the transfer leaves
    pub source: MintId,
    /// The mint the transfer lands in
    pub destination: MintId,
    /// The amount, in sat
    #[schemars(with = "u64")]
    pub amount: bitcoin::Amount,
    /// The operation of the onchain send; `onchain_send_success` carries
    /// the txid once the source mint has broadcast
    pub operation: OperationId,
}

/// Why `rebalance` could not decide or make a transfer.
#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum RebalanceError {
    #[error("The broker has fewer than two mints")]
    TooFewMints,
    #[error(transparent)]
    Fee(#[from] onchain::FeeError),
    #[error(transparent)]
    Receive(#[from] onchain::ReceiveError),
    #[error(transparent)]
    Send(#[from] onchain::SendError),
}
