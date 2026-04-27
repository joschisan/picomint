use aleph_bft::UncheckedSignedUnit;
use picomint_core::TransactionId;
use picomint_core::session_outcome::{AcceptedItem, SignedSessionOutcome};
use picomint_core::transaction::ConsensusItem;
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::{consensus_value, table};

/// Newtype around the in-progress aleph unit so we can satisfy the orphan
/// rule and implement `redb::Value` via consensus encoding.
#[derive(Debug, Clone, Encodable, Decodable)]
pub struct AlephUnit(pub UncheckedSignedUnit<Vec<ConsensusItem>>);

consensus_value!(AlephUnit);

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

table!(
    ALEPH_UNITS,
    u64 => AlephUnit,
    "aleph-units",
);
