use std::collections::BTreeMap;

use bitcoin::address::NetworkUnchecked;
use clap::Args;
use lightning_invoice::Bolt11Invoice;
use picomint_client::ecash::Ecash;
use picomint_core::config::MintId;
use picomint_core::core::Account;
use picomint_core::core::OperationId;
use picomint_core::ecash::Denomination;
use picomint_core::invite::InviteCode;
use picomint_core::{Amount, secp256k1};
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, serde_as};

/// Filename of the gateway's admin CLI Unix socket, inside `DATA_DIR`.
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
pub const ROUTE_CLIENT_ECASH_RECEIVE: &str = "/client/ecash/receive";
pub const ROUTE_CLIENT_ONCHAIN_SEND_FEE: &str = "/client/onchain/send-fee";
pub const ROUTE_CLIENT_ONCHAIN_SEND: &str = "/client/onchain/send";
pub const ROUTE_CLIENT_ONCHAIN_RECEIVE: &str = "/client/onchain/receive";

// --- /info ---

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct InfoResponse {
    /// Lightning node public key (LDK node id).
    pub lightning_pk: secp256k1::PublicKey,
    /// Iroh public key the gateway accepts on for the picomint API.
    /// Mint nodes register this via `module lightning gateway add`.
    pub gateway_pk: picomint_core::lightning::gateway::GatewayPk,
    pub alias: String,
    pub network: String,
    pub block_height: u64,
    pub synced_to_chain: bool,
}

// --- /mnemonic ---

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MnemonicResponse {
    pub mnemonic: Vec<String>,
}

// --- /ldk/balances ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LdkBalancesResponse {
    /// Everything the on-chain wallet holds, including unconfirmed funds and
    /// the anchor reserve below.
    pub total_onchain_balance_sat: u64,
    /// The share of the on-chain wallet we can spend right now: sufficiently
    /// confirmed, minus the anchor reserve below.
    pub spendable_onchain_balance_sat: u64,
    /// On-chain funds withheld so we can always bump a channel's anchor output
    /// to get its force-close transaction confirmed.
    pub total_anchor_channels_reserve_sat: u64,
    /// What our usable channels can still receive.
    pub total_inbound_capacity_sat: u64,
    /// What our usable channels can still send.
    pub total_outbound_capacity_sat: u64,
    /// The largest single payment each usable channel will still forward,
    /// summed. Sits below the outbound capacity, which one payment cannot
    /// exhaust.
    pub total_next_outbound_htlc_limit_sat: u64,
    /// What we could claim across all channels, including the timelocked side
    /// of a channel that has already been closed.
    pub total_lightning_balance_sat: u64,
    /// Funds from closed channels that are on their way back to the on-chain
    /// wallet but are not swept into it yet.
    pub total_pending_closure_balance_sat: u64,
}

// --- /ldk/channel/open ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkChannelOpenRequest {
    pub pubkey: secp256k1::PublicKey,
    pub host: String,
    pub channel_size_sat: u64,
    #[arg(long, default_value_t = 0)]
    pub push_amount_sat: u64,
    /// Announce the channel to the network so other nodes can route through
    /// it. Requires the node to be configured with a listening address and an
    /// alias.
    #[arg(long)]
    #[serde(default)]
    pub announce: bool,
}

// --- /ldk/channel/close ---

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkChannelCloseRequest {
    /// Channel to close, as reported by `channel list`. Identifies the channel
    /// rather than the peer, since a peer may hold several.
    #[serde_as(as = "DisplayFromStr")]
    pub user_channel_id: u128,
    /// Peer the channel is with.
    pub pubkey: secp256k1::PublicKey,
    #[arg(long)]
    #[serde(default)]
    pub force: bool,
}

// --- /ldk/channel/list ---

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LdkChannelListResponse {
    pub channels: Vec<ChannelInfo>,
}

#[serde_as]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChannelInfo {
    /// Local identifier of the channel, as accepted by the close and splice
    /// commands.
    #[serde_as(as = "DisplayFromStr")]
    pub user_channel_id: u128,
    pub remote_pubkey: secp256k1::PublicKey,
    pub remote_alias: Option<String>,
    pub remote_address: Option<String>,
    pub channel_size_sat: u64,
    pub outbound_liquidity_sat: u64,
    pub next_outbound_htlc_limit_sat: u64,
    pub inbound_liquidity_sat: u64,
    pub is_usable: bool,
    pub is_outbound: bool,
    /// Whether the channel is, or once confirmed will be, publicly announced.
    pub is_announced: bool,
    pub funding_txid: Option<bitcoin::Txid>,
}

// --- /ldk/channel/splice-in ---

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkChannelSpliceInRequest {
    /// Channel to splice on-chain funds into, as reported by `channel list`.
    /// Identifies the channel rather than the peer, since a peer may hold
    /// several.
    #[serde_as(as = "DisplayFromStr")]
    pub user_channel_id: u128,
    /// Peer the channel is with.
    pub pubkey: secp256k1::PublicKey,
    /// On-chain funds to add to the channel, in satoshi.
    pub amount_sat: u64,
}

// --- /ldk/channel/splice-out ---

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkChannelSpliceOutRequest {
    /// Channel to splice funds out of, as reported by `channel list`.
    /// Identifies the channel rather than the peer, since a peer may hold
    /// several.
    #[serde_as(as = "DisplayFromStr")]
    pub user_channel_id: u128,
    /// Peer the channel is with.
    pub pubkey: secp256k1::PublicKey,
    /// Destination on-chain address for the spliced-out funds.
    pub address: bitcoin::Address<NetworkUnchecked>,
    /// Amount to remove from the channel, in satoshi (must not exceed the
    /// channel's outbound capacity).
    pub amount_sat: u64,
}

// --- /ldk/lightning/probe ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkLightningProbeRequest {
    /// The node to probe a route towards.
    pub node_id: secp256k1::PublicKey,
    /// The amount to find paths for, in millisatoshis.
    pub amount_msat: u64,
}

// --- /ldk/onchain/receive ---

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LdkOnchainReceiveResponse {
    pub address: bitcoin::Address<NetworkUnchecked>,
}

// --- /ldk/onchain/send ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkOnchainSendRequest {
    pub address: bitcoin::Address<NetworkUnchecked>,
    pub amount: bitcoin::Amount,
    #[arg(long)]
    pub sat_per_vbyte: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LdkOnchainSendResponse {
    pub txid: bitcoin::Txid,
}

// --- /ldk/lightning/receive ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkLightningReceiveRequest {
    pub amount_msat: u64,
    #[arg(long)]
    pub expiry_secs: Option<u32>,
    #[arg(long)]
    pub description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LdkLightningReceiveResponse {
    pub invoice: String,
}

// --- /ldk/lightning/send ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkLightningSendRequest {
    pub invoice: Bolt11Invoice,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LdkLightningSendResponse {
    pub preimage: String,
}

// --- /ldk/peer/connect ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkPeerConnectRequest {
    pub pubkey: secp256k1::PublicKey,
    pub host: String,
}

// --- /ldk/peer/disconnect ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkPeerDisconnectRequest {
    pub pubkey: secp256k1::PublicKey,
}

// --- /ldk/peer/list ---

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LdkPeerListResponse {
    pub peers: Vec<PeerInfo>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PeerInfo {
    pub node_id: secp256k1::PublicKey,
    pub address: String,
    pub is_connected: bool,
}

// --- /client/add ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientAddRequest {
    pub invite: InviteCode,
}

// --- /client/remove ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientRemoveRequest {
    pub mint: MintId,
}

// --- /client/balance ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientBalanceRequest {
    pub mint: MintId,
    pub account: Account,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientBalanceResponse {
    pub balance_msat: Amount,
}

// --- /client/list ---

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientListResponse {
    pub mints: Vec<MintInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MintInfo {
    pub mint: MintId,
    pub mint_name: String,
}

// --- /client/config ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientConfigRequest {
    pub mint: MintId,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct ClientConfigResponse {
    pub config: serde_json::Value,
}

// --- /query ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct QueryRequest {
    /// Read-only SQL run against the analytics db, e.g.
    /// "SELECT * FROM gateway_send ORDER BY ts DESC LIMIT 10"
    pub query: String,
}

/// One JSON object per row, keyed by result column name — the same shape
/// `sqlite3 --json` prints.
pub type QueryResponse = Vec<serde_json::Map<String, serde_json::Value>>;

// --- /client/ecash/count ---

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

// --- /client/ecash/send ---

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

// --- /client/ecash/receive ---

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

// --- /client/onchain/send-fee ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientOnchainSendFeeRequest {
    pub mint: MintId,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientOnchainSendFeeResponse {
    pub fee: bitcoin::Amount,
}

// --- /client/onchain/send ---

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

// --- /client/onchain/receive ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct ClientOnchainReceiveRequest {
    pub mint: MintId,
    pub account: Account,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientOnchainReceiveResponse {
    pub address: bitcoin::Address<bitcoin::address::NetworkUnchecked>,
}
