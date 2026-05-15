use picomint_bft::{Cosig, Round, Unit};
use picomint_core::expiry;
use picomint_core::session;
use picomint_core::tx::ConsensusItem;
use picomint_core::{PeerId, TransactionId};
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
