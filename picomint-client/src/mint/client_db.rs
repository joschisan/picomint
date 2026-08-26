use picomint_core::core::{Account, OperationId};

use super::SpendableNote;

// Tracks that a `receive(ecash)` has been started for this deterministic
// [`OperationId`]. Keyed by the bundle alone, so the guard spans every
// account: notes can only be reissued once, and an attempt to receive the
// same bundle into a second account is rejected rather than left to fail
// against already-spent notes.
client_table!(
    ReceiveOperationTable,
    OperationId => (),
    "mint-receive-operation",
);

// Every account's notes share one table, split by the key's leading
// [`Account`]. Nonces derive from per-account secrets, so two accounts can
// never produce the same note.
client_table!(
    NoteTable,
    (Account, SpendableNote) => (),
    "mint-note",
);

// Next unused issuance counter, per account. One space serves every
// denomination, so a transaction consumes one counter per output. Read and
// bumped in the same dbtx that builds the transaction carrying those outputs,
// so a counter is only consumed once its blinded message is actually
// committed to.
//
// Restore rewrites an account's counter to the high-water mark it scanned to;
// a restored wallet that resumed from zero would re-derive nonces the
// federation has already signed.
client_table!(
    CounterTable,
    Account => u64,
    "mint-counter",
);
