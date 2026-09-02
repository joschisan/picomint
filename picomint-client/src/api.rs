//! Core federation wire methods — federation semantics on top of the pooled
//! [`FederationApi`], which lives in `picomint_rpc` and is used directly.
//!
//! The per-module wire methods are free functions over the same handle in
//! each module's `api.rs`; the module path carries the prefix the method
//! names used to (`wallet::api::send_fee`, not `api.wallet_send_fee()`).

pub use picomint_rpc::api::FederationApi;

use picomint_core::PeerId;
use picomint_core::expiry::ExpiryStatus;
use picomint_core::methods::{
    CoreMethod, ExpiryStatusRequest, ExpiryStatusResponse, LivenessRequest, LivenessResponse,
    SubmitTxRequest, SubmitTxResponse,
};
use picomint_core::module::Method;
use picomint_core::tx::{Transaction, TxError};

/// Submit a transaction and await the final outcome. The server long-
/// polls until the tx is either accepted or becomes invalid.
pub async fn submit_tx(api: &FederationApi, tx: Transaction) -> Result<(), TxError> {
    api.request_current_consensus_retry::<SubmitTxResponse>(Method::Core(CoreMethod::SubmitTx(
        SubmitTxRequest { tx },
    )))
    .await
    .outcome
}

/// Lightweight liveness check — succeeds if a threshold of guardians is
/// reachable.
pub async fn liveness(api: &FederationApi) -> anyhow::Result<LivenessResponse> {
    api.request_current_consensus(Method::Core(CoreMethod::Liveness(LivenessRequest)))
        .await
}

/// Single-peer liveness check — succeeds if `peer` answers. Useful for
/// surfacing per-peer connection status (e.g. dashboards) where the
/// threshold-consensus variant would mask which peer is offline.
pub async fn liveness_peer(api: &FederationApi, peer: PeerId) -> anyhow::Result<LivenessResponse> {
    api.request_single_peer(Method::Core(CoreMethod::Liveness(LivenessRequest)), peer)
        .await
}

/// Fetch the federation's announced expiry status, threshold-
/// consensus verified. Returns `Some(_)` only if a threshold of
/// guardians return the byte-equal value, `None` if all guardians
/// agree no expiry has been announced.
pub async fn expiry_status(api: &FederationApi) -> anyhow::Result<Option<ExpiryStatus>> {
    api.request_current_consensus::<ExpiryStatusResponse>(Method::Core(CoreMethod::ExpiryStatus(
        ExpiryStatusRequest,
    )))
    .await
    .map(|r| r.status)
}
