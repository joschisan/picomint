use picomint_bft::{UnitEnvelope, UnitHash};
use picomint_core::expiry;
use picomint_core::session;
use picomint_core::tx::ConsensusItem;
use picomint_core::version::ConsensusVersion;
use picomint_core::{NumPeers, PeerId, TransactionId};
use picomint_encoding::{Decodable, Encodable};
use picomint_sqlite::{DbRead, table};

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
    u64 => session::SignedSessionOutcome,
    "signed-session-outcome",
);

// Highest consensus version each peer has announced support for. A peer
// votes once per upgrade and never downwards, so a missing entry means the
// peer has not upgraded past the version the federation was created at.
table!(
    ConsensusVersionVoteTable,
    PeerId => ConsensusVersion,
    "consensus-version-vote",
);

/// The consensus version the federation currently runs at.
///
/// Sorted ascending and indexed at `max_evil()`, so `2f + 1` peers voted for
/// at least this version — a threshold can run it — and `f + 1` voted for at
/// most it, so at least one honest guardian announced it. The vec is padded
/// rather than indexed short because a peer that has not voted still counts:
/// it supports `default_version` and nothing beyond, and that has to weigh on
/// the result the same as a vote would.
pub fn consensus_version(
    dbtx: &impl DbRead,
    num_peers: NumPeers,
    default_version: ConsensusVersion,
) -> ConsensusVersion {
    let mut versions = dbtx.iter(&ConsensusVersionVoteTable, |r| {
        r.map(|(_, version)| version).collect::<Vec<_>>()
    });

    while versions.len() < num_peers.total() {
        versions.push(default_version);
    }

    versions.sort_unstable();

    versions[num_peers.max_evil()]
}

// This guardian's locally-announced expiry status. Mutated by the admin
// dashboard; read by [`crate::consensus::rpc::expiry_status`] and
// returned over the wire so a threshold of guardians must agree on the
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
    pub user_limit: u64,
}

// Expiration date and user limit for each invite code this guardian issued,
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
    [u8; 16] => u64,
    "invite-user-count",
);
