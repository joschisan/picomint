use picomint_bft::{Cosig, Round, Unit};
use picomint_core::expiry;
use picomint_core::session;
use picomint_core::tx::ConsensusItem;
use picomint_core::{PeerId, TransactionId};
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::{DbWrite, table};

table!(
    AcceptedItemTable,
    u64 => session::AcceptedItem,
    "accepted-item",
);

// bft tables — owned by the daemon, lent to `picomint_bft::Engine`
// via `Engine::new`. Cleaned up at session boundary by
// `drop_bft_tables` alongside `AcceptedItemTable`.

table!(
    BftUnitsTable,
    (Round, PeerId) => Unit<ConsensusItem>,
    "bft-units",
);

table!(
    BftCosigsTable,
    (Round, PeerId, PeerId) => Cosig,
    "bft-cosigs",
);

/// Drop the daemon-owned bft session tables. Called from
/// `complete_session` next to the `AcceptedItemTable` cleanup.
pub fn drop_bft_tables(dbtx: &impl DbWrite) {
    dbtx.delete_table(&BftUnitsTable);
    dbtx.delete_table(&BftCosigsTable);
}

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

picomint_redb::consensus_value!(InviteMeta);

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
