//! Routes and request/response types of the node daemon's admin CLI. Every
//! response type carries a JSON Schema, printed by the CLI under
//! `--schema`, so its doc lines are the operator-facing description of each
//! field.

use chrono::NaiveDate;
use clap::Args;
use picomint_core::NodeId;
use picomint_core::bitcoin::Txid;
use picomint_core::config::{MintId, NodeConfig, NodeSetupCode};
use picomint_core::error::ErrorCode;
use picomint_core::expiry::ExpiryStatus;
use picomint_core::invite::InviteCode;
use picomint_core::lightning::gateway::GatewayPk;
use picomint_core::onchain::TxInfo;
use picomint_core::version::ConsensusVersion;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tss::SecretKeyShare;

/// Served in every phase of the node's life; everything else is phase-bound.
pub const ROUTE_STATUS: &str = "/status";

// Setup routes
pub const ROUTE_SETUP_INIT: &str = "/setup/init";
pub const ROUTE_SETUP_ADD: &str = "/setup/add";
pub const ROUTE_SETUP_RESET: &str = "/setup/reset";
pub const ROUTE_SETUP_CONFIRM: &str = "/setup/confirm";
pub const ROUTE_SETUP_RESTORE: &str = "/setup/restore";

// Consensus routes
pub const ROUTE_BITCOIND: &str = "/bitcoind";
pub const ROUTE_INVITE: &str = "/invite";
pub const ROUTE_BACKUP: &str = "/backup";
pub const ROUTE_EXPIRY_SET: &str = "/expiry/set";
pub const ROUTE_EXPIRY_CLEAR: &str = "/expiry/clear";
pub const ROUTE_EXPIRY_STATUS: &str = "/expiry/status";

// Module routes
pub const ROUTE_ONCHAIN_STATUS: &str = "/onchain/status";
pub const ROUTE_ONCHAIN_PENDING: &str = "/onchain/pending";
pub const ROUTE_ONCHAIN_HISTORY: &str = "/onchain/history";
pub const ROUTE_ONCHAIN_RUGPULL: &str = "/onchain/rugpull";
pub const ROUTE_GATEWAY_ADD: &str = "/gateway/add";
pub const ROUTE_GATEWAY_REMOVE: &str = "/gateway/remove";
pub const ROUTE_GATEWAY_LIST: &str = "/gateway/list";

// --- /status ---

/// Which phase the node is in, with what an operator needs at that point.
/// `phase` is the first thing to look at: every other command is served by
/// exactly one phase and fails in the others.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "phase")]
pub enum NodeStatus {
    /// The setup ceremony: this node collects the other nodes' setup codes
    /// until every node has confirmed the node set
    Setup(SetupPhase),
    /// Distributed key generation is running; nothing can be done until it
    /// completes and the node moves to consensus
    Dkg(DkgPhase),
    /// The mint is up and processing transactions
    Consensus(Box<ConsensusPhase>),
}

/// The ceremony state as this node sees it. Nothing here is shared with
/// the other nodes except through the setup codes the operators exchange.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SetupPhase {
    /// This node's setup code, to hand to every other node's operator: its
    /// name and iroh public key, plus the mint name and size if this node
    /// set them. Absent until `setup init` has run
    pub setup_code: Option<NodeSetupCode>,
    /// This node's name as given to `setup init`; absent until then
    pub node_name: Option<String>,
    /// The mint's name, once any setup code carrying it has been added or
    /// this node set it; absent until then
    pub mint_name: Option<String>,
    /// The number of nodes in the mint, from the same source as
    /// `mint_name`; absent until then
    pub mint_size: Option<u8>,
    /// Names of the other nodes whose setup codes have been added so far;
    /// `setup confirm` needs `mint_size - 1` of them
    pub nodes: Vec<String>,
}

/// Key generation is running. It completes once every node has confirmed
/// the same node set and come online, so a node sits here while it waits
/// for the others.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DkgPhase {
    /// This node's setup code, for any node whose operator still needs it
    pub setup_code: NodeSetupCode,
}

/// The mint is running. Everything here is public; the private keys are
/// only ever returned by `backup`. Every value is the mint's, agreed by a
/// threshold of nodes, or this node's own; what this node's bitcoind
/// backend reports is under `bitcoind`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ConsensusPhase {
    /// The mint's name, as set at the ceremony
    pub mint_name: String,
    /// The mint's id, the hash of its consensus config
    pub mint_id: MintId,
    /// The Bitcoin network the mint runs on, read off the backends at the
    /// ceremony: `bitcoin`, `signet` or `regtest`
    pub network: String,
    /// This node's index in the mint
    pub node_id: NodeId,
    /// This node's name, as given to `setup init`
    pub node_name: String,
    /// The revision of the consensus rules the mint runs: the highest
    /// version a threshold of nodes' binaries support. Nodes on an older
    /// binary halt once it advances past them
    pub consensus_version: ConsensusVersion,
    /// Consensus sessions closed since the mint started. A session is one
    /// batch of ordered items signed off by a threshold of nodes; the count
    /// is the mint's own clock, and a node catching up replays sessions
    /// towards it
    pub session_count: u32,
    /// The Bitcoin block height the mint agrees on: every node votes the
    /// tip of its own backend, and the vote a threshold of nodes have
    /// reached wins, so one backend running ahead or behind the others
    /// moves nothing. What `created` in the transaction history refers to
    pub block_height: u32,
    /// The next block the mint wallet votes on: every block below it has
    /// been processed for deposits and for the confirmations of the mint's
    /// own transactions. Nodes read a block from their backends once it is
    /// 6 confirmations deep and vote its relevant transactions into the
    /// consensus log, so this trails `block_height` by about that. A node
    /// that was down alone catches up from the log and reads no old
    /// blocks; the wallet only falls further behind when the mint as a
    /// whole stops voting, and then resumes from here, which makes this the
    /// height a pruned backend must still serve
    pub onchain_block_height: u32,
    /// Every other node with the state of this node's connection to it
    pub nodes: Vec<NodeInfo>,
}

// --- status: nodes ---

/// One other node of the mint, as seen from this node's p2p connection to
/// it.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct NodeInfo {
    /// The node's index in the mint
    pub id: NodeId,
    /// The node's name, as it gave it at the ceremony
    pub name: String,
    /// Whether this node currently holds a live connection to it
    pub connected: bool,
    /// How the connection currently runs: `direct` over a hole-punched UDP
    /// path, or `relay` through an iroh relay server while no direct path
    /// exists. Absent while disconnected
    pub transport: Option<String>,
    /// Where the connection goes: the node's socket address on a direct
    /// path, the relay's URL on a relayed one. Absent while disconnected
    pub remote_addr: Option<String>,
    /// Round-trip time of the connection's current path in milliseconds,
    /// as iroh measures it; over a relay this spans both legs. Absent while
    /// disconnected
    pub rtt_ms: Option<u64>,
}

// --- /bitcoind ---

/// This node's own Bitcoin Core backend, read live, as opposed to the
/// mint's consensus view of the chain. These are the values this node votes
/// into consensus; the votes of a threshold of nodes make `block_height`
/// in `status` and the fee rate in `onchain status`. Fails while the
/// backend is unreachable, with what went wrong.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct BitcoindResponse {
    /// The network the backend runs, told by its block at height 1; the
    /// mint's `network` was read from it at the ceremony and the two must
    /// agree
    pub network: String,
    /// The backend's chain tip, from `getblockcount`: this node's vote for
    /// the consensus `block_height`, which trails it while the other nodes'
    /// backends are behind
    pub block_height: u32,
    /// The backend's fee estimate in sat/vB, `estimatesmartfee` for the
    /// next block in conservative mode, floored at 1: this node's vote for
    /// the consensus fee rate. Fixed at 1 on regtest. Absent while the
    /// backend has no estimate, as on a freshly synced node
    pub fee_rate_sat_per_vb: Option<u32>,
    /// The backend's initial block download progress from 0 to 1, from
    /// `getblockchaininfo`; the node waits for it to reach the tip before
    /// it joins consensus
    pub sync_progress: f64,
}

// --- /setup/init ---

#[derive(Clone, Debug, Serialize, Deserialize, Args)]
pub struct SetupInitRequest {
    /// This node's name, shown to the other nodes and to clients; nodes are
    /// numbered by the sort order of their setup codes, which starts with
    /// the name
    pub name: String,
    /// The mint's name; set by exactly one node, whose setup code carries it
    /// to the others
    #[arg(long)]
    pub mint_name: Option<String>,
    /// Number of nodes in the mint: 4, 7, 10, 13, 16, 19 or 22, one more
    /// than a multiple of three so a third can fail; set by the same node
    #[arg(long)]
    pub mint_size: Option<u8>,
}

/// The code the other nodes' operators need from this one.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SetupInitResponse {
    /// This node's setup code: a `picomint`-prefixed base32 string carrying
    /// its name, its iroh public key and, if this node set them, the mint
    /// name and size. Running `setup init` again with the same arguments
    /// prints the same code
    pub setup_code: NodeSetupCode,
}

// --- /setup/add ---

#[derive(Clone, Debug, Serialize, Deserialize, Args)]
pub struct SetupAddRequest {
    /// Another node's setup code, as printed by its `setup init`; adding a
    /// code twice is harmless
    pub setup_code: NodeSetupCode,
}

/// Confirms which node the code belonged to.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SetupAddResponse {
    /// The name of the node whose setup code was added
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
    /// Days until the invite code expires, at most 365; this node stops
    /// serving the mint config for it after that
    #[arg(long = "days", default_value_t = DEFAULT_INVITE_EXPIRY_DAYS)]
    pub expiry_days: u64,
    /// How many clients may join with this code; each config download
    /// counts as one, and this node alone keeps the count, since it alone
    /// serves the code
    #[arg(long = "users", default_value_t = DEFAULT_INVITE_USER_LIMIT)]
    pub user_limit: u32,
}

/// A code for users to join the mint with.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct InviteResponse {
    /// The invite code: it names the mint and this node, which serves the
    /// mint config to whoever presents the code until it expires or its
    /// user limit is reached
    pub invite: InviteCode,
}

// --- /backup ---

/// The node's whole config as `backup` prints it and `setup restore`
/// takes back unchanged. Secret: pipe it into a file, never to a
/// terminal.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct BackupResponse {
    /// The mint's consensus config under `consensus` and this node's
    /// private keys under `private`; its shape is the node's, not this
    /// CLI's, and nothing but `setup restore` reads it
    #[schemars(schema_with = "node_config_schema")]
    pub config: NodeConfig,
}

fn node_config_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "object",
        "description": "The node's whole config: the mint's consensus config under `consensus` and this node's private keys under `private`. Opaque to everything but `setup restore`."
    })
}

// --- /expiry/* ---

/// How far ahead a wind-down may be announced, in days. Far enough for any
/// orderly migration, close enough that a mistyped year is refused.
pub const EXPIRY_DAYS_LIMIT: u64 = 730;

#[derive(Clone, Debug, Serialize, Deserialize, Args)]
pub struct ExpirySetRequest {
    /// The wind-down day as `YYYY-MM-DD`; the daemon announces midnight
    /// UTC of that day, so nodes that enter the same date announce
    /// byte-equal values. Must lie in the future and at most 730 days out
    pub date: NaiveDate,
    /// Invite code of the successor mint for users to migrate to; every
    /// node must enter the exact same one, or none
    #[arg(long)]
    pub successor: Option<InviteCode>,
}

/// This node's own announcement, not the mint's: clients act on an expiry
/// only once a threshold of nodes announce the same values.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ExpiryStatusResponse {
    /// What this node announces; absent when it announces nothing
    pub expiry: Option<ExpiryStatus>,
}

// --- /onchain/status ---

/// The mint wallet at a glance. The wallet is one UTXO that every mint
/// transaction spends into its successor, so the wallet's history is a
/// chain and `tx_tip` is its head.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct OnchainStatusResponse {
    /// Value of the wallet UTXO in sat: everything the mint holds onchain,
    /// which backs every user's ecash. 0 until the first deposit
    pub total_value_sat: u64,
    /// Id of the transaction whose output is the wallet UTXO: the newest
    /// mint transaction, confirmed or still pending. Absent until the first
    /// deposit has established the wallet
    #[schemars(with = "Option<String>")]
    pub tx_tip: Option<Txid>,
    /// Mint transactions ever created, pending ones included; the next one
    /// gets this as its `index`
    pub tx_count: u64,
    /// The consensus fee rate in sat/vB: of the fee estimates the nodes'
    /// backends report, the lowest one that a threshold of nodes still
    /// consider sufficient, so no minority can push it. What the mint's
    /// next transaction pays per vbyte and what the send fee quoted to
    /// clients is sized from. Absent until a threshold of nodes have voted,
    /// as right after setup or while backends are syncing
    pub feerate_sat_per_vb: Option<u32>,
}

// --- /onchain/pending ---

/// Mint transactions broadcast but not yet confirmed, newest first. A
/// transaction is pending from the block it was created in until a
/// threshold of nodes have seen it 6 confirmations deep.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PendingResponse {
    /// The pending transactions, newest first
    pub txs: Vec<TxInfo>,
}

// --- /onchain/history ---

/// Every mint transaction ever created, oldest first.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct HistoryResponse {
    /// The transactions in creation order; the last one is `tx_tip`
    pub txs: Vec<TxInfo>,
}

// --- /onchain/rugpull ---

/// This node's share of the key to the mint wallet, for draining it once
/// the mint has expired: the share is tweaked for the current wallet UTXO,
/// and additive tweaks commute with Lagrange interpolation, so
/// `picomint-rugpull` interpolates a threshold of nodes' shares into the
/// UTXO's key. Secret: pipe it into a file, never to a terminal.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RugpullResponse {
    /// The node this share belongs to, its evaluation point
    pub node: NodeId,
    /// The share, tweaked for the wallet UTXO at the time of export, so it
    /// is only valid while `tx_tip` stays what it is now
    #[schemars(with = "String")]
    pub sks: SecretKeyShare,
}

// --- /gateway/* ---

#[derive(Clone, Debug, Serialize, Deserialize, Args)]
pub struct LightningGatewayAddRequest {
    /// The gateway's `gateway_pk`, as printed by `picomint-gateway-cli info`
    pub pk: GatewayPk,
    /// The name this node lists the gateway under
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Args)]
pub struct LightningGatewayRemoveRequest {
    /// The gateway's `gateway_pk`, as printed by `picomint-gateway-cli info`
    pub pk: GatewayPk,
}

/// The gateways this node recommends. Clients of the mint use a gateway
/// once a threshold of nodes recommend it, so this list is one vote.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct LightningGatewayListResponse {
    /// This node's recommendations
    pub gateways: Vec<LightningGatewayInfo>,
}

/// One recommended gateway.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct LightningGatewayInfo {
    /// The gateway's identity, its `gateway_pk`
    pub pk: GatewayPk,
    /// The name this node lists it under; local to this node
    pub name: String,
}

// --- errors ---

/// Why `status` has nothing to say for a moment.
#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum StatusError {
    #[error("Key generation has just completed; the node is starting consensus")]
    DkgCompleting,
}

/// Why `setup init` refused.
#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum SetupInitError {
    #[error("The node name is empty")]
    EmptyNodeName,
    #[error("The mint name is empty")]
    EmptyMintName,
    #[error("The node that sets the mint name sets the mint size too")]
    MintSizeMissing,
    #[error("Mint size must be at least 4")]
    MintSizeTooSmall,
    #[error(
        "The node has already been initialized; `setup init` with the same arguments repeats its code"
    )]
    AlreadyInitialized,
}

/// Why `setup add` refused the code.
#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum SetupAddError {
    #[error("The node has not been initialized yet; run `setup init` first")]
    NotInitialized,
    #[error("This is the node's own setup code")]
    OwnSetupCode,
    #[error("The mint name has already been set to {0}")]
    MintNameAlreadySet(String),
    #[error("The mint size has already been set to {0}")]
    MintSizeAlreadySet(u8),
}

/// Why `setup confirm` did not start key generation.
#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum SetupConfirmError {
    #[error("The node has not been initialized yet; run `setup init` first")]
    NotInitialized,
    #[error("Mint size must be at least 4")]
    MintSizeTooSmall,
    #[error("The mint size is {expected} but {got} setup codes are in, this node's included")]
    WrongNodeCount { expected: u8, got: usize },
    #[error("No setup code carries the mint name; one node has to set it")]
    MintNameMissing,
    #[error("Failed to determine the network from the bitcoin backend: {0}")]
    Bitcoind(String),
    #[error("Picomint is experimental software and refuses to run a mint on mainnet")]
    Mainnet,
    #[error("The setup has already completed")]
    Completed,
}

/// Why `setup restore` refused the backup.
#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum SetupRestoreError {
    #[error("The restored config failed validation: {0}")]
    InvalidConfig(String),
    #[error("The setup has already completed")]
    Completed,
}

/// Why the backend could not be read.
#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum BitcoindError {
    #[error("The bitcoin backend did not answer: {0}")]
    Unreachable(String),
}

/// Why no invite code was issued.
#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum InviteError {
    #[error("Invite codes are issued once the mint has reached consensus on a block height")]
    NoBlockHeight,
    #[error("The expiry must be at most 365 days out")]
    ExpiryTooFar,
}

/// Why the expiry announcement was refused.
#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum ExpirySetError {
    #[error("The expiry date must be in the future")]
    NotInFuture,
    #[error("The expiry date must be at most 730 days out")]
    TooFar,
}

/// Why the rugpull secret was not exported.
#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum RugpullError {
    #[error("The mint wallet has not received funds yet, so there is nothing to drain")]
    WalletEmpty,
}

/// Why the gateway was not added to this node's recommendations.
#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum GatewayAddError {
    #[error("The gateway is already recommended")]
    AlreadyRecommended,
}

/// Why the gateway was not removed from this node's recommendations.
#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum GatewayRemoveError {
    #[error("The gateway is not recommended")]
    NotRecommended,
}
