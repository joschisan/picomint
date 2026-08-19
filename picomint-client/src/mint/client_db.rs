use picomint_core::core::OperationId;
use picomint_core::mint::Denomination;

use super::SpendableNote;

// Tracks that a `receive(ecash)` has been started for this deterministic
// [`OperationId`]. Used to make `receive` idempotent.
client_table!(
    ReceiveOperationTable,
    OperationId => (),
    "mint-receive-operation",
);

client_table!(
    NoteTable,
    SpendableNote => (),
    "mint-note",
);

// Next unused issuance counter per denomination. Read and bumped in the same
// dbtx that builds the transaction carrying the outputs, so a counter is only
// consumed once its blinded message is actually committed to.
//
// Recovery rewrites these to the high-water mark it scanned to; a restored
// wallet that resumed from zero would re-derive nonces the federation has
// already signed.
client_table!(
    CounterTable,
    Denomination => u64,
    "mint-counter",
);
