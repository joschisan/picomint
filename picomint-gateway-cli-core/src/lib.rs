use std::collections::BTreeMap;

use bitcoin::address::NetworkUnchecked;
use clap::Args;
use lightning_invoice::Bolt11Invoice;
use picomint_client::ecash::ECash;
use picomint_core::config::FederationId;
use picomint_core::invite::InviteCode;
use picomint_core::ecash::Denomination;
use picomint_core::{Amount, secp256k1};
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, serde_as};

/// Filename of the gateway's admin CLI Unix socket, inside `DATA_DIR`.
/// The daemon binds and the CLI connects at `{DATA_DIR}/{CLI_SOCKET_FILENAME}`.
pub const CLI_SOCKET_FILENAME: &str = "cli.sock";

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
pub const ROUTE_LDK_LN_RECEIVE: &str = "/ldk/ln/receive";
pub const ROUTE_LDK_LN_SEND: &str = "/ldk/ln/send";
pub const ROUTE_LDK_LN_PROBE: &str = "/ldk/ln/probe";
pub const ROUTE_LDK_PEER_CONNECT: &str = "/ldk/peer/connect";
pub const ROUTE_LDK_PEER_DISCONNECT: &str = "/ldk/peer/disconnect";
pub const ROUTE_LDK_PEER_LIST: &str = "/ldk/peer/list";

// Analytics
pub const ROUTE_QUERY: &str = "/query";

// Federation management
pub const ROUTE_FEDERATION_ADD: &str = "/federation/add";
pub const ROUTE_FEDERATION_LIST: &str = "/federation/list";
pub const ROUTE_FEDERATION_CONFIG: &str = "/federation/config";
pub const ROUTE_FEDERATION_BALANCE: &str = "/federation/balance";
pub const ROUTE_FEDERATION_REMOVE: &str = "/federation/remove";

// Per-federation module commands
pub const ROUTE_FEDERATION_MODULE_ECASH_COUNT: &str = "/federation/module/ecash/count";
pub const ROUTE_FEDERATION_MODULE_ECASH_SEND: &str = "/federation/module/ecash/send";
pub const ROUTE_FEDERATION_MODULE_ECASH_RECEIVE: &str = "/federation/module/ecash/receive";
pub const ROUTE_FEDERATION_MODULE_WALLET_SEND_FEE: &str = "/federation/module/wallet/send-fee";
pub const ROUTE_FEDERATION_MODULE_WALLET_SEND: &str = "/federation/module/wallet/send";
pub const ROUTE_FEDERATION_MODULE_WALLET_RECEIVE: &str = "/federation/module/wallet/receive";

// --- /info ---

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct InfoResponse {
    /// Lightning node public key (LDK node id).
    pub lightning_pk: secp256k1::PublicKey,
    /// Iroh public key the gateway accepts on for the picomint API.
    /// Federation guardians register this via `module ln gateway add`.
    pub gateway_pk: picomint_core::ln::gateway::GatewayPk,
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

// --- /ldk/ln/probe ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkLnProbeRequest {
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

// --- /ldk/ln/receive ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkLnReceiveRequest {
    pub amount_msat: u64,
    #[arg(long)]
    pub expiry_secs: Option<u32>,
    #[arg(long)]
    pub description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LdkLnReceiveResponse {
    pub invoice: String,
}

// --- /ldk/ln/send ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct LdkLnSendRequest {
    pub invoice: Bolt11Invoice,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LdkLnSendResponse {
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

// --- /federation/add ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct FederationAddRequest {
    pub invite: InviteCode,
}

// --- /federation/remove ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct FederationRemoveRequest {
    pub federation: FederationId,
}

// --- /federation/balance ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct FederationBalanceRequest {
    #[arg(long = "id")]
    pub federation: Option<FederationId>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FederationBalanceResponse {
    pub balance_msat: Amount,
}

// --- /federation/list ---

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FederationListResponse {
    pub federations: Vec<FederationInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FederationInfo {
    pub federation: FederationId,
    pub federation_name: String,
}

// --- /federation/config ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct FederationConfigRequest {
    #[arg(long = "id")]
    pub federation: Option<FederationId>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct FederationConfigResponse {
    pub config: serde_json::Value,
}

// --- /query ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct QueryRequest {
    /// Read-only SQL run against the analytics db, e.g.
    /// "SELECT * FROM outgoing_payments ORDER BY started_at DESC LIMIT 10"
    pub query: String,
}

/// One JSON object per row, keyed by result column name — the same shape
/// `sqlite3 --json` prints.
pub type QueryResponse = Vec<serde_json::Map<String, serde_json::Value>>;

// --- /federation/module/ecash/count ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct FederationMintCountRequest {
    #[arg(long = "id")]
    pub federation: Option<FederationId>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FederationMintCountResponse {
    /// Count of held ecash notes keyed by denomination.
    pub counts: BTreeMap<Denomination, u64>,
}

// --- /federation/module/ecash/send ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct FederationMintSendRequest {
    pub amount: bitcoin::Amount,
    #[arg(long = "id")]
    pub federation: Option<FederationId>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FederationMintSendResponse {
    pub ecash: ECash,
}

// --- /federation/module/ecash/receive ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct FederationMintReceiveRequest {
    pub ecash: ECash,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FederationMintReceiveResponse {
    pub amount: Amount,
}

// --- /federation/module/wallet/send-fee ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct FederationWalletSendFeeRequest {
    #[arg(long = "id")]
    pub federation: Option<FederationId>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FederationWalletSendFeeResponse {
    pub fee: bitcoin::Amount,
}

// --- /federation/module/wallet/send ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct FederationWalletSendRequest {
    pub address: bitcoin::Address<NetworkUnchecked>,
    pub amount: bitcoin::Amount,
    #[arg(long)]
    pub fee: Option<bitcoin::Amount>,
    #[arg(long = "id")]
    pub federation: Option<FederationId>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FederationWalletSendResponse {
    pub txid: bitcoin::Txid,
}

// --- /federation/module/wallet/receive ---

#[derive(Debug, Clone, Serialize, Deserialize, Args)]
pub struct FederationWalletReceiveRequest {
    #[arg(long = "id")]
    pub federation: Option<FederationId>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FederationWalletReceiveResponse {
    pub address: bitcoin::Address<bitcoin::address::NetworkUnchecked>,
}
