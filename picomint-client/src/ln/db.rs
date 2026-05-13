use picomint_core::core::OperationId;

client_table!(
    IncomingContractStreamIndexTable,
    () => u64,
    "ln-incoming-contract-stream-index",
);

// Tracks that a send operation has been started for this [`OperationId`].
// Used to reject duplicate pay attempts for the same invoice (the operation id is
// derived from the invoice payment hash).
client_table!(
    SendOperationTable,
    OperationId => (),
    "ln-send-operation",
);
