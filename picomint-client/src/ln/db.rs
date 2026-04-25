use picomint_core::core::OperationId;
use picomint_redb::table;

table!(
    INCOMING_CONTRACT_STREAM_INDEX,
    () => u64,
    "ln-incoming-contract-stream-index",
);

// Tracks that a send operation has been started for this [`OperationId`].
// Used to reject duplicate pay attempts for the same invoice (the op id is
// derived from the invoice payment hash).
table!(
    SEND_OPERATION,
    OperationId => (),
    "ln-send-operation",
);
