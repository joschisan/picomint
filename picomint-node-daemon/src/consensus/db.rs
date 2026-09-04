use picomint_bft::{UnitEnvelope, UnitHash};
use picomint_core::expiry;
use picomint_core::session;
use picomint_core::tx::ConsensusItem;
use picomint_core::version::ConsensusVersion;
use picomint_core::{NodeId, NumNodesExt, TransactionId};
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::{DbRead, table};

use crate::consensus::server::Server;

table!(
    AcceptedItemTable,
    u64 => session::AcceptedItem,
    "accepted-item",
);

// bft table — owned by the daemon, lent to `picomint_bft::Engine`
// via `Engine::new`. Cleaned up at session boundary by
// `complete_session` alongside `AcceptedItemTable`.

table!(
    BftUnitsTable,
    UnitHash => UnitEnvelope<ConsensusItem>,
    "bft-units",
);

table!(
    AcceptedTxTable,
    TransactionId => (),
    "accepted-tx",
);

table!(
    SignedSessionOutcomeTable,
    u32 => session::SignedSessionOutcome,
    "signed-session-outcome",
);

// Latest block count each node has voted for. Votes only ever increase, so a
// missing entry means the node has not voted since the mint was created.
table!(
    BlockCountVoteTable,
    NodeId => u32,
    "block-count-vote",
);

/// The consensus block count the mint currently runs at.
///
/// Sorted descending and indexed at `threshold() - 1`, so any threshold of
/// correct nodes can increase the consensus block count and any consensus
/// block count has been confirmed by a threshold of nodes.
pub fn consensus_block_count(server: &Server, dbtx: &impl DbRead) -> u32 {
    let num_nodes = server.cfg.consensus.nodes.to_num_nodes();

    let mut counts: Vec<u32> = dbtx.iter(&BlockCountVoteTable, |r| r.map(|(_, v)| v).collect());

    assert!(counts.len() <= num_nodes.total());

    counts.sort_unstable();

    counts.reverse();

    counts.get(num_nodes.threshold() - 1).copied().unwrap_or(0)
}

// Highest consensus version each node has announced support for. A node
// votes once per upgrade and never downwards, so a missing entry means the
// node has not upgraded past the version the mint was created at.
table!(
    ConsensusVersionVoteTable,
    NodeId => ConsensusVersion,
    "consensus-version-vote",
);

/// The consensus version the mint currently runs at.
///
/// Sorted ascending and indexed at `max_evil()`, so `2f + 1` nodes voted for
/// at least this version — a threshold can run it — and `f + 1` voted for at
/// most it, so at least one honest node announced it. The vec is padded
/// rather than indexed short because a node that has not voted still counts:
/// it supports `default_version` and nothing beyond, and that has to weigh on
/// the result the same as a vote would.
pub fn consensus_version(server: &Server, dbtx: &impl DbRead) -> ConsensusVersion {
    let num_nodes = server.cfg.consensus.nodes.to_num_nodes();

    let mut versions = dbtx.iter(&ConsensusVersionVoteTable, |r| {
        r.map(|(_, version)| version).collect::<Vec<_>>()
    });

    while versions.len() < num_nodes.total() {
        versions.push(server.cfg.consensus.default_version);
    }

    versions.sort_unstable();

    versions[num_nodes.max_evil()]
}

// This node's locally-announced expiry status. Mutated by the admin
// dashboard; read by [`crate::consensus::rpc::expiry_status`] and
// returned over the wire so a threshold of nodes must agree on the
// byte-equal value before clients trust it.
table!(
    ExpiryStatusTable,
    () => expiry::ExpiryStatus,
    "expiry-status",
);

/// Metadata an invite code's issuer keeps for it, keyed by the opaque invite
/// id embedded in the invite code.
#[derive(Clone, Debug, Encodable, Decodable)]
pub struct InviteMeta {
    /// Unix timestamp in seconds after which the invite code is expired.
    pub expires_at: u64,
    /// Maximum number of users that may download the config via this invite.
    pub user_limit: u32,
}

// Expiration date and user limit for each invite code this node issued,
// keyed by invite id. Written by the dashboard / CLI create flow, read when
// serving the config to enforce the invite code's limits.
table!(
    InviteMetaTable,
    [u8; 16] => InviteMeta,
    "invite-meta",
);

// Number of config downloads counted against each invite id so far; a missing
// entry means zero. Incremented in the same transaction that serves the config.
table!(
    InviteUserCountTable,
    [u8; 16] => u32,
    "invite-user-count",
);
