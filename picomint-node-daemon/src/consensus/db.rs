use std::collections::BTreeMap;

use picomint_bft::{Unit, UnitHash};
use picomint_core::expiry;
use picomint_core::secp256k1::schnorr;
use picomint_core::session;
use picomint_core::tx::ConsensusItem;
use picomint_core::version::ConsensusVersion;
use picomint_core::{NodeId, NumNodesExt, TransactionId};
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::{DbRead, table};

use crate::consensus::server::Server;

// Every accepted item under its session and its dense position within
// it, written once as it is accepted and never moved: a session's items
// are read back only to serve a node catching up, or to resume after a
// crash.
table!(
    AcceptedItemTable,
    (u32, u64) => session::AcceptedItem,
    "accepted-item",
);

// The bft delivery position the running session resumes from after a
// crash: every position below it was processed, accepted or rejected.
// Cleared with the signatures at the cut.
table!(
    ResumeIndexTable,
    () => u64,
    "resume-index",
);

// The bft engine's three tables — declared here, lent to
// `picomint_bft::Engine` via `Engine::new`, and cleared at the session
// boundary by `finalize_session`.

table!(
    BftUnitTable,
    UnitHash => Unit,
    "bft-unit",
);

table!(
    BftUnitDataTable,
    UnitHash => Vec<ConsensusItem>,
    "bft-unit-data",
);

table!(
    BftUnitSignatureTable,
    UnitHash => schnorr::Signature,
    "bft-unit-signature",
);

table!(
    AcceptedTxidTable,
    TransactionId => (),
    "accepted-txid",
);

// A threshold of node signatures over a closed session's header. The
// signed outcome a node catching up asks for is assembled from this and
// the session's items on request.
table!(
    SessionSignaturesTable,
    u32 => BTreeMap<NodeId, schnorr::Signature>,
    "session-signatures",
);

// Latest block height each node has voted for. Votes only ever increase, so
// a missing entry means the node has not voted since the mint was created.
table!(
    BlockHeightVoteTable,
    NodeId => u32,
    "block-height-vote",
);

/// The consensus block height the mint currently runs at.
///
/// Sorted descending and indexed at `threshold() - 1`, so any threshold of
/// correct nodes can increase the consensus block height and any consensus
/// block height has been confirmed by a threshold of nodes.
pub fn consensus_block_height(server: &Server, dbtx: &impl DbRead) -> u32 {
    let num_nodes = server.cfg.consensus.nodes.to_num_nodes();

    let mut heights: Vec<u32> = dbtx.iter(&BlockHeightVoteTable, |r| r.map(|(_, v)| v).collect());

    assert!(heights.len() <= num_nodes.total());

    heights.sort_unstable();

    heights.reverse();

    heights.get(num_nodes.threshold() - 1).copied().unwrap_or(0)
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
// CLI; read by [`crate::consensus::rpc::expiry_status`] and
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
// keyed by invite id. Written by the CLI create flow, read when
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
