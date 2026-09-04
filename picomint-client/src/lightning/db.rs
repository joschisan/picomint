use picomint_core::config::MintId;
use picomint_core::core::OperationId;
use picomint_core::lightning::gateway::GatewayPk;
use picomint_redb::table;

table!(
    IncomingContractStreamCursorTable,
    MintId => u64,
    "lightning-incoming-contract-stream-cursor",
);

// The mint's announced gateway pks, mirrored to disk by
// `update_gateway_pks`. On a cold start they are probed straight away by
// `update_gateway_info` to repopulate the in-memory pool, so the client need
// not wait on the threshold-consensus gateway query before `select_gateway`
// can return. The probed `GatewayInfo` itself stays in memory, never persisted.
table!(
    GatewayPkTable,
    (MintId, GatewayPk) => (),
    "lightning-gateway-pk",
);

// Tracks that a send operation has been started for this [`OperationId`].
// Used to reject duplicate pay attempts for the same invoice (the operation id is
// derived from the invoice payment hash).
table!(
    SendOperationIdTable,
    (MintId, OperationId) => (),
    "lightning-send-operation-id",
);
