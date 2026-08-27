//! The client's federation API: [`picomint_rpc::api::FederationApi`] plus the
//! wire methods that are federation semantics rather than pooling.
//!
//! The per-module helpers (`ln_*`, `mint_*`, `wallet_*`, `gw_*`) hang off this
//! type in each module's `api.rs`, which is why it wraps the rpc one rather
//! than being it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;

use futures::stream::BoxStream;
use iroh::{Endpoint, PublicKey};
use picomint_core::expiry::ExpiryStatus;
use picomint_core::methods::{
    CoreMethod, ExpiryStatusRequest, ExpiryStatusResponse, LivenessRequest, LivenessResponse,
    SubmitTxRequest, SubmitTxResponse,
};
use picomint_core::module::Method;
use picomint_core::tx::{Transaction, TxError};
use picomint_core::{NumPeers, PeerId};
use picomint_encoding::Decodable;
use picomint_rpc::connection::ConnStatus;
use picomint_rpc::query::QueryStrategy;

#[derive(Clone, Debug)]
pub struct FederationApi(picomint_rpc::api::FederationApi);

impl FederationApi {
    pub fn new(endpoint: Endpoint, peer_node_ids: BTreeMap<PeerId, PublicKey>) -> Self {
        Self(picomint_rpc::api::FederationApi::new(
            endpoint,
            peer_node_ids,
        ))
    }

    pub fn all_peers(&self) -> BTreeSet<PeerId> {
        self.0.all_peers()
    }

    pub fn num_peers(&self) -> NumPeers {
        self.0.num_peers()
    }

    pub fn endpoint(&self) -> &Endpoint {
        self.0.endpoint()
    }

    pub fn connection_status_stream(&self) -> BoxStream<'static, BTreeMap<PeerId, ConnStatus>> {
        self.0.connection_status_stream()
    }

    pub async fn request_single_peer<R: Decodable>(
        &self,
        method: Method,
        peer: PeerId,
    ) -> anyhow::Result<R> {
        self.0.request_single_peer(method, peer).await
    }

    pub async fn request_with_strategy<P: Decodable + Send + 'static, F: Debug>(
        &self,
        strategy: impl QueryStrategy<P, F> + Send,
        method: Method,
    ) -> anyhow::Result<F> {
        self.0.request_with_strategy(strategy, method).await
    }

    pub async fn request_with_strategy_retry<P: Decodable + Send + 'static, F: Debug>(
        &self,
        strategy: impl QueryStrategy<P, F> + Send,
        method: Method,
    ) -> F {
        self.0.request_with_strategy_retry(strategy, method).await
    }

    pub async fn request_current_consensus<R>(&self, method: Method) -> anyhow::Result<R>
    where
        R: Decodable + Eq + Debug + Clone + Send + 'static,
    {
        self.0.request_current_consensus(method).await
    }

    pub async fn request_current_consensus_retry<R>(&self, method: Method) -> R
    where
        R: Decodable + Eq + Debug + Clone + Send + 'static,
    {
        self.0.request_current_consensus_retry(method).await
    }

    /// Submit a transaction and await the final outcome. The server long-
    /// polls until the tx is either accepted or becomes invalid.
    pub async fn submit_tx(&self, tx: Transaction) -> Result<(), TxError> {
        self.request_current_consensus_retry::<SubmitTxResponse>(Method::Core(
            CoreMethod::SubmitTx(SubmitTxRequest { tx }),
        ))
        .await
        .outcome
    }

    /// Lightweight liveness check — succeeds if a threshold of guardians is
    /// reachable.
    pub async fn liveness(&self) -> anyhow::Result<LivenessResponse> {
        self.request_current_consensus(Method::Core(CoreMethod::Liveness(LivenessRequest)))
            .await
    }

    /// Single-peer liveness check — succeeds if `peer` answers. Useful for
    /// surfacing per-peer connection status (e.g. dashboards) where the
    /// threshold-consensus variant would mask which peer is offline.
    pub async fn liveness_peer(&self, peer: PeerId) -> anyhow::Result<LivenessResponse> {
        self.request_single_peer(Method::Core(CoreMethod::Liveness(LivenessRequest)), peer)
            .await
    }

    /// Fetch the federation's announced expiry status, threshold-
    /// consensus verified. Returns `Some(_)` only if a threshold of
    /// guardians return the byte-equal value, `None` if all guardians
    /// agree no expiry has been announced.
    pub async fn expiry_status(&self) -> anyhow::Result<Option<ExpiryStatus>> {
        self.request_current_consensus::<ExpiryStatusResponse>(Method::Core(
            CoreMethod::ExpiryStatus(ExpiryStatusRequest),
        ))
        .await
        .map(|r| r.status)
    }
}
