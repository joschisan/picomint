use picomint_bft::{Entry, Round};
use picomint_core::expiration;
use picomint_core::session;
use picomint_core::tx::ConsensusItem;
use picomint_core::{PeerId, TransactionId};
use picomint_redb::table;

table!(
    AcceptedItemTable,
    u64 => session::AcceptedItem,
    "accepted-item",
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

// One row per `(round, creator)` slot holding the `Entry` for that slot.
// Overwritten in place as the entry's signature set grows; iterating in
// natural key order yields `(round, peer)` lex order — the order the
// engine expects for recover.
table!(
    BftUnitsTable,
    (Round, PeerId) => Entry<ConsensusItem>,
    "bft-units",
);

// This guardian's locally-announced expiration status. Mutated by the admin
// dashboard; read by [`crate::consensus::rpc::expiration_status`] and
// returned over the wire so a threshold of guardians must agree on the
// byte-equal value before clients trust it.
table!(
    ExpirationStatusTable,
    () => expiration::ExpirationStatus,
    "expiration-status",
);
