//! Routes and request/response types of the gateway daemon's admin CLI.
//! Every response type carries a JSON Schema, printed by the CLI under
//! `--schema`, so its doc lines are the operator-facing description of each
//! field. The `client` commands mirror the client daemon's and deliberately
//! keep their own copies of the types — the two surfaces are allowed to
//! drift, and the compiler polices each against the client library on its
//! own.

use std::collections::BTreeMap;

use bitcoin::address::NetworkUnchecked;
use clap::Args;
use lightning::ln::msgs::SocketAddress;
use lightning_invoice::Bolt11Invoice;
use picomint_client::ecash::Ecash;
use picomint_core::config::MintId;
use picomint_core::core::Account;
use picomint_core::core::OperationId;
use picomint_core::ecash::Denomination;
use picomint_core::error::ErrorCode;
use picomint_core::invite::InviteCode;
use picomint_core::lightning::gateway::GatewayPk;
use picomint_core::{Amount, secp256k1};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, serde_as};
use thiserror::Error;

// Top-level
pub const ROUTE_INFO: &str = "/info";
pub const ROUTE_MNEMONIC: &str = "/mnemonic";

// LDK node management
pub const ROUTE_LDK_BALANCES: &str = "/ldk/balances";
pub const ROUTE_LDK_CHANNEL_OPEN: &str = "/ldk/channel/open";
pub const ROUTE_LDK_CHANNEL_CLOSE: &str = "/ldk/channel/close";
pub const ROUTE_LDK_CHANNEL_LIST: &str = "/ldk/channel/list";
pub const ROUTE_LDK_CHANNEL_SPLICE_IN: &str = "/ldk/channel/splice-in";
pub const ROUTE_LDK_CHANNEL_SPLICE_OUT: &str = "/ldk/channel/splice-out";
pub const ROUTE_LDK_ONCHAIN_RECEIVE: &str = "/ldk/onchain/receive";
pub const ROUTE_LDK_ONCHAIN_SEND: &str = "/ldk/onchain/send";
pub const ROUTE_LDK_LIGHTNING_RECEIVE: &str = "/ldk/lightning/receive";
pub const ROUTE_LDK_LIGHTNING_SEND: &str = "/ldk/lightning/send";
pub const ROUTE_LDK_LIGHTNING_PROBE: &str = "/ldk/lightning/probe";
pub const ROUTE_LDK_PEER_CONNECT: &str = "/ldk/peer/connect";
pub const ROUTE_LDK_PEER_DISCONNECT: &str = "/ldk/peer/disconnect";
pub const ROUTE_LDK_PEER_LIST: &str = "/ldk/peer/list";

// Analytics
pub const ROUTE_QUERY: &str = "/query";

// Mint management
pub const ROUTE_CLIENT_ADD: &str = "/client/add";
pub const ROUTE_CLIENT_LIST: &str = "/client/list";
pub const ROUTE_CLIENT_CONFIG: &str = "/client/config";
pub const ROUTE_CLIENT_BALANCE: &str = "/client/balance";
pub const ROUTE_CLIENT_REMOVE: &str = "/client/remove";

// Client module commands
pub const ROUTE_CLIENT_ECASH_COUNT: &str = "/client/ecash/count";
pub const ROUTE_CLIENT_ECASH_SEND: &str = "/client/ecash/send";
pub const ROUTE_CLIENT_ECASH_SEND_MAX: &str = "/client/ecash/send-max";
pub const ROUTE_CLIENT_ECASH_RECEIVE: &str = "/client/ecash/receive";
pub const ROUTE_CLIENT_ONCHAIN_SEND_FEE: &str = "/client/onchain/send-fee";
pub const ROUTE_CLIENT_ONCHAIN_SEND: &str = "/client/onchain/send";
pub const ROUTE_CLIENT_ONCHAIN_SEND_MAX: &str = "/client/onchain/send-max";
pub const ROUTE_CLIENT_ONCHAIN_RECEIVE: &str = "/client/onchain/receive";

// --- /info ---

/// The gateway's two identities and the state of its Lightning node. The
/// gateway is one LDK node facing the Lightning Network and one client of
/// every mint it serves; `lightning_pk` names the former, `gateway_pk` the
/// latter.
#[derive(Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct InfoResponse {
    /// The LDK node's id, a hex secp256k1 public key: what Lightning peers
    /// and LSPs connect to and open channels with
    #[schemars(with = "String")]
    pub lightning_pk: secp256k1::PublicKey,
    /// The gateway's identity towards mints and their clients, its iroh
    /// public key. Mint nodes recommend it via `picomint-node-cli gateway
    /// add`
    pub gateway_pk: GatewayPk,
    /// The alias the LDK node announces to the Lightning Network; fixed to
    /// `picomint-gateway-daemon`
    pub alias: String,
    /// The Bitcoin network the LDK node runs on, from `NETWORK`; every mint
    /// the gateway serves has to run on it
    pub network: String,
    /// The LDK node's best block height as its chain source, bitcoind or
    /// esplora, reports it; each mint keeps its own consensus height
    pub block_height: u64,
    /// Whether the LDK wallet has completed a chain sync since the daemon
    /// started; balances and channel states are stale until it has
    pub synced_to_chain: bool,
}

// --- /mnemonic ---

/// The seed. Secret: pipe it into a file, never to a terminal.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct MnemonicResponse {
    /// The twelve BIP39 words the LDK onchain wallet and every mint balance
    /// derive from; channel balances do not
    pub mnemonic: Vec<String>,
}

// --- /ldk/balances ---

/// Everything the LDK node holds, onchain and in channels, in sat. Ecash
/// held in mints is not part of this; `client balance` reports it per
/// mint.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LdkBalancesResponse {
    /// Everything the on-chain wallet holds, including unconfirmed funds and
    /// the anchor reserve below
    pub total_onchain_balance_sat: u64,
    /// The share of the on-chain wallet we can spend right now: sufficiently
    /// confirmed, minus the anchor reserve below
    pub spendable_onchain_balance_sat: u64,
    /// On-chain funds withheld so we can always bump a channel's anchor output
    /// to get its force-close transaction confirmed
    pub total_anchor_channels_reserve_sat: u64,
    /// What our usable channels can still receive
    pub total_inbound_capacity_sat: u64,
    /// What our usable channels can still send
    pub total_outbound_capacity_sat: u64,
    /// The largest single payment each usable channel will still forward,
    /// summed. Sits below the outbound capacity, which one payment cannot
    /// exhaust
    pub total_next_outbound_htlc_limit_sat: u64,
    /// What we could claim across all channels, including the timelocked side
    /// of a channel that has already been closed
    pub total_lightning_balance_sat: u64,
    /// Funds from closed channels that are on their way back to the on-chain
    /// wallet but are not swept into it yet
    pub total_pending_closure_balance_sat: u64,
}

// --- /ldk/channel/open ---

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkChannelOpenRequest {
    /// The peer's node id, hex
    pub pubkey: secp256k1::PublicKey,
    /// The peer's `host:port`: an IP, a hostname or an onion address
    #[serde_as(as = "DisplayFromStr")]
    pub host: SocketAddress,
    /// The channel's capacity with its denomination, e.g. "1000000 sat",
    /// funded from the LDK onchain wallet
    pub channel_size: bitcoin::Amount,
    /// An amount with its denomination handed to the peer as its starting
    /// balance in the channel; nothing if omitted
    #[arg(long)]
    pub push_amount: Option<bitcoin::Amount>,
    /// Announce the channel to the network so other nodes can route through
    /// it. Requires the node to be configured with a listening address and an
    /// alias
    #[arg(long)]
    #[serde(default)]
    pub announce: bool,
}

// --- /ldk/channel/close ---

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkChannelCloseRequest {
    /// Channel to close, as reported by `channel list`. Identifies the channel
    /// rather than the peer, since a peer may hold several
    #[serde_as(as = "DisplayFromStr")]
    pub user_channel_id: u128,
    /// Peer the channel is with
    pub pubkey: secp256k1::PublicKey,
    /// Close unilaterally instead of negotiating with the peer; our balance
    /// then comes back after the channel's timelock
    #[arg(long)]
    #[serde(default)]
    pub force: bool,
}

// --- /ldk/channel/list ---

/// Every channel the LDK node has, open or still confirming.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct LdkChannelListResponse {
    /// The channels, in no particular order
    pub channels: Vec<ChannelInfo>,
}

/// One channel with its liquidity in both directions, in sat.
#[serde_as]
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ChannelInfo {
    /// Local identifier of the channel, as accepted by the close and splice
    /// commands; a decimal string
    #[serde_as(as = "DisplayFromStr")]
    #[schemars(with = "String")]
    pub user_channel_id: u128,
    /// The peer's node id, hex
    #[schemars(with = "String")]
    pub remote_pubkey: secp256k1::PublicKey,
    /// The peer's alias from its node announcement; absent if it announces
    /// none or the network graph does not know the peer
    pub remote_alias: Option<String>,
    /// The peer's socket address as the LDK node has it on record; absent
    /// if the peer is not in `peer list`
    pub remote_address: Option<String>,
    /// The channel's capacity in sat
    pub channel_size_sat: u64,
    /// What this node can send over the channel right now, in sat: its
    /// balance less the reserve and in-flight payments
    pub outbound_liquidity_sat: u64,
    /// The largest single payment the channel will forward right now, in
    /// sat; at or below `outbound_liquidity_sat`
    pub next_outbound_htlc_limit_sat: u64,
    /// What the peer can send this way right now, in sat
    pub inbound_liquidity_sat: u64,
    /// Whether the channel can carry payments right now: funding confirmed
    /// and the peer connected
    pub is_usable: bool,
    /// Whether this node opened and funded the channel
    pub is_outbound: bool,
    /// Whether the channel is, or once confirmed will be, publicly announced
    pub is_announced: bool,
    /// Id of the funding transaction, hex; absent only while it has not been
    /// created yet
    #[schemars(with = "Option<String>")]
    pub funding_txid: Option<bitcoin::Txid>,
}

// --- /ldk/channel/splice-in ---

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkChannelSpliceInRequest {
    /// Channel to splice on-chain funds into, as reported by `channel list`.
    /// Identifies the channel rather than the peer, since a peer may hold
    /// several
    #[serde_as(as = "DisplayFromStr")]
    pub user_channel_id: u128,
    /// Peer the channel is with
    pub pubkey: secp256k1::PublicKey,
    /// The on-chain funds to add to the channel, with their denomination,
    /// e.g. "100000 sat"
    pub amount: bitcoin::Amount,
}

// --- /ldk/channel/splice-out ---

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkChannelSpliceOutRequest {
    /// Channel to splice funds out of, as reported by `channel list`.
    /// Identifies the channel rather than the peer, since a peer may hold
    /// several
    #[serde_as(as = "DisplayFromStr")]
    pub user_channel_id: u128,
    /// Peer the channel is with
    pub pubkey: secp256k1::PublicKey,
    /// Destination on-chain address for the spliced-out funds
    pub address: bitcoin::Address<NetworkUnchecked>,
    /// The amount to remove from the channel, with its denomination, e.g.
    /// "100000 sat"; at most the channel's outbound capacity
    pub amount: bitcoin::Amount,
}

// --- /ldk/lightning/probe ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkLightningProbeRequest {
    /// The node to probe a route towards, hex
    pub node_id: secp256k1::PublicKey,
    /// The amount to find paths for, with its denomination, e.g. "10000 sat"
    pub amount: bitcoin::Amount,
}

// --- /ldk/onchain/receive ---

/// An address of the LDK onchain wallet.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct LdkOnchainReceiveResponse {
    /// A fresh receive address of the LDK onchain wallet; funds sent here
    /// are what `channel open` spends
    #[schemars(with = "String")]
    pub address: bitcoin::Address<NetworkUnchecked>,
}

// --- /ldk/onchain/send ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkOnchainSendRequest {
    /// The destination address, on the LDK node's network
    pub address: bitcoin::Address<NetworkUnchecked>,
    /// The amount with its denomination, e.g. "100000 sat" or "0.001 BTC"
    pub amount: bitcoin::Amount,
    /// The fee rate to pay, in sat/vB
    #[arg(long)]
    pub sat_per_vbyte: u64,
}

/// The broadcast transaction.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct LdkOnchainSendResponse {
    /// Id of the transaction the LDK wallet broadcast, hex
    #[schemars(with = "String")]
    pub txid: bitcoin::Txid,
}

// --- /ldk/lightning/receive ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkLightningReceiveRequest {
    /// The invoice amount with its denomination, e.g. "10000 sat"
    pub amount: bitcoin::Amount,
    /// Seconds until the invoice expires; 3600 if omitted
    #[arg(long)]
    pub expiry_secs: Option<u32>,
    /// The invoice description; empty if omitted
    #[arg(long)]
    pub description: Option<String>,
}

/// An invoice of the LDK node itself.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct LdkLightningReceiveResponse {
    /// The bolt11 invoice; paying it funds the LDK node's channel balance,
    /// not a mint
    pub invoice: String,
}

// --- /ldk/lightning/send ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkLightningSendRequest {
    /// The bolt11 invoice to pay from the LDK node's own channel balance
    pub invoice: Bolt11Invoice,
}

/// Proof that the invoice was paid. The command waits for the payment to
/// settle and fails if it does.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct LdkLightningSendResponse {
    /// The payment preimage, hex
    pub preimage: String,
}

// --- /ldk/peer/connect ---

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkPeerConnectRequest {
    /// The peer's node id, hex
    pub pubkey: secp256k1::PublicKey,
    /// The peer's `host:port`: an IP, a hostname or an onion address
    #[serde_as(as = "DisplayFromStr")]
    pub host: SocketAddress,
}

// --- /ldk/peer/disconnect ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkPeerDisconnectRequest {
    /// The peer's node id, hex
    pub pubkey: secp256k1::PublicKey,
}

// --- /ldk/peer/list ---

/// Every Lightning peer the LDK node knows, connected or not.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct LdkPeerListResponse {
    /// The peers, in no particular order
    pub peers: Vec<PeerInfo>,
}

/// One Lightning peer.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct PeerInfo {
    /// The peer's node id, hex
    #[schemars(with = "String")]
    pub node_id: secp256k1::PublicKey,
    /// The socket address the LDK node reaches the peer at
    pub address: String,
    /// Whether the connection is up right now; LDK reconnects on its own
    pub is_connected: bool,
}

// --- /client/add ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientAddRequest {
    /// The mint's invite code, as printed by a node's `invite`; the mint has
    /// to run on this gateway's `NETWORK`
    pub invite: InviteCode,
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
    /// payments route from `primary`, the others are reserves
    pub account: Account,
}

/// An account's ecash balance in one mint.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct ClientBalanceResponse {
    /// The sum of the notes the account holds, in msat
    pub balance_msat: Amount,
}

// --- /client/list ---

/// The mints the gateway has added, whether or not their nodes recommend
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

// --- /query ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct QueryRequest {
    /// Read-only SQL run against the analytics db, e.g.
    /// "SELECT * FROM gateway_send ORDER BY ts DESC LIMIT 10"
    pub query: String,
}

/// The rows the query returned: one JSON object per row, keyed by result
/// column name, the same shape `sqlite3 --json` prints. Column types
/// follow the analytics schema: amounts as integers, msat except in the
/// onchain tables, which are sat; hashes, ids and keys as text.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct QueryResponse(pub Vec<serde_json::Map<String, serde_json::Value>>);

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
    /// acceptance shows up in the analytics as `core_tx_accept`
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

// --- errors ---

/// Why an `ldk` command did nothing: LDK refused it, with its reason.
#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum LdkError {
    #[error("LDK refused: {0}")]
    Ldk(String),
}

/// Why `ldk lightning receive` produced no invoice.
#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum LdkReceiveError {
    #[error("The invoice description is too long")]
    InvalidDescription,
    #[error("LDK refused: {0}")]
    Ldk(String),
}

/// Why `ldk lightning send` did not pay.
#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum LdkSendError {
    #[error("LDK refused: {0}")]
    Ldk(String),
    #[error("The payment failed")]
    PaymentFailed,
}
