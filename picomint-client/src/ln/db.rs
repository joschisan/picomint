use picomint_core::core::OperationId;
use picomint_core::ln::gateway::GatewayPk;

client_table!(
    IncomingContractStreamIndexTable,
    () => u64,
    "ln-incoming-contract-stream-index",
);

// The federation's announced gateway pks, mirrored to disk by
// `refresh_gateways`. On a cold start they are probed straight away to
// repopulate the in-memory info map, so the client need not wait on the
// threshold-consensus gateway query before `select_gateway` can return.
// The probed `GatewayInfo` itself stays in memory and is never persisted.
client_table!(
    GatewayPkTable,
    GatewayPk => (),
    "ln-gateway-pk",
);

// Tracks that a send operation has been started for this [`OperationId`].
// Used to reject duplicate pay attempts for the same invoice (the operation id is
// derived from the invoice payment hash).
client_table!(
    SendOperationTable,
    OperationId => (),
    "ln-send-operation",
);
