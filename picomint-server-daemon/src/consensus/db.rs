use picomint_aleph_bft::{Entry, Round};
use picomint_core::session_outcome::{AcceptedItem, SignedSessionOutcome};
use picomint_core::transaction::ConsensusItem;
use picomint_core::{PeerId, TransactionId};
use picomint_redb::table;

table!(
    ACCEPTED_ITEM,
    u64 => AcceptedItem,
    "accepted-item",
);

table!(
    ACCEPTED_TRANSACTION,
    TransactionId => (),
    "accepted-transaction",
);

table!(
    SIGNED_SESSION_OUTCOME,
    u64 => SignedSessionOutcome,
    "signed-session-outcome",
);

// One row per `(round, creator)` slot holding the `Entry` for that slot.
// Overwritten in place as the entry's signature set grows; iterating in
// natural key order yields `(round, peer)` lex order — the order the
// engine expects for restore.
table!(
    ALEPH_UNITS,
    (Round, PeerId) => Entry<ConsensusItem>,
    "aleph-units",
);
