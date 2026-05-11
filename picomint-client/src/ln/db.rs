use picomint_core::core::OperationId;
use picomint_core::ln::gateway_api::{GatewayInfo, GatewayPk};
use picomint_redb::table;

// Local cache of `GatewayInfo` for every gateway in the federation's
// announced list. Refreshed by `refresh_gateways` at startup (and on demand
// from tests). Read synchronously from `select_gateway` — no live RPC in
// the hot path.
table!(
    GATEWAY_INFO,
    GatewayPk => GatewayInfo,
    "ln-gateway-info",
);

table!(
    INCOMING_CONTRACT_STREAM_INDEX,
    () => u64,
    "ln-incoming-contract-stream-index",
);

// Tracks that a send operation has been started for this [`OperationId`].
// Used to reject duplicate pay attempts for the same invoice (the operation id is
// derived from the invoice payment hash).
table!(
    SEND_OPERATION,
    OperationId => (),
    "ln-send-operation",
);
