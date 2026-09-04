//! Core mint wire methods — mint semantics on top of the pooled
//! [`MintApi`], which lives in `picomint_rpc` and is used directly.
//!
//! The per-module wire methods are free functions over the same handle in
//! each module's `api.rs`; the module path carries the prefix the method
//! names used to (`onchain::api::send_fee`, not `api.onchain_send_fee()`).

pub use picomint_rpc::api::MintApi;

use picomint_core::NodeId;
use picomint_core::expiry::ExpiryStatus;
use picomint_core::methods::{
    BlockCountRequest, BlockCountResponse, CoreMethod, ExpiryStatusRequest, ExpiryStatusResponse,
    LivenessRequest, LivenessResponse, SubmitTxRequest, SubmitTxResponse,
};
use picomint_core::module::Method;
use picomint_core::tx::{Transaction, TxError};

/// Submit a transaction and await the final outcome. The server long-
/// polls until the tx is either accepted or becomes invalid.
pub async fn submit_tx(api: &MintApi, tx: Transaction) -> Result<(), TxError> {
    api.request_current_consensus_retry::<SubmitTxResponse>(Method::Core(CoreMethod::SubmitTx(
        SubmitTxRequest { tx },
    )))
    .await
    .outcome
}

/// Fetch the mint's consensus block count, which trails the chain
/// tip by the confirmation finality delay.
pub async fn block_count(api: &MintApi) -> anyhow::Result<u32> {
    api.request_current_consensus::<BlockCountResponse>(Method::Core(CoreMethod::BlockCount(
        BlockCountRequest,
    )))
    .await
    .map(|resp| resp.count)
}

/// Lightweight liveness check — succeeds if a threshold of nodes is
/// reachable.
pub async fn liveness(api: &MintApi) -> anyhow::Result<LivenessResponse> {
    api.request_current_consensus(Method::Core(CoreMethod::Liveness(LivenessRequest)))
        .await
}

/// Single-node liveness check — succeeds if `node` answers. Useful for
/// surfacing per-node connection status (e.g. dashboards) where the
/// threshold-consensus variant would mask which node is offline.
pub async fn liveness_node(api: &MintApi, node: NodeId) -> anyhow::Result<LivenessResponse> {
    api.request_single_node(Method::Core(CoreMethod::Liveness(LivenessRequest)), node)
        .await
}

/// Fetch the mint's announced expiry status, threshold-
/// consensus verified. Returns `Some(_)` only if a threshold of
/// nodes return the byte-equal value, `None` if all nodes
/// agree no expiry has been announced.
pub async fn expiry_status(api: &MintApi) -> anyhow::Result<Option<ExpiryStatus>> {
    api.request_current_consensus::<ExpiryStatusResponse>(Method::Core(CoreMethod::ExpiryStatus(
        ExpiryStatusRequest,
    )))
    .await
    .map(|r| r.status)
}
