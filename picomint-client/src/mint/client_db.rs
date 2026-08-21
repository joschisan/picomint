use picomint_core::core::OperationId;

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

// Next unused issuance counter. One space serves every denomination, so a
// transaction consumes one counter per output. Read and bumped in the same
// dbtx that builds the transaction carrying those outputs, so a counter is
// only consumed once its blinded message is actually committed to.
//
// Restore rewrites this to the high-water mark it scanned to; a restored
// wallet that resumed from zero would re-derive nonces the federation has
// already signed.
client_table!(
    CounterTable,
    () => u64,
    "mint-counter",
);
