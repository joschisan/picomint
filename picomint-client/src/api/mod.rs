use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::future::pending;

use anyhow::{Context, anyhow};
use iroh::{Endpoint, PublicKey};
use picomint_core::backoff::{Retryable, networking_backoff};
use picomint_core::expiry::ExpiryStatus;
use picomint_core::methods::{
    CoreMethod, ExpiryStatusRequest, ExpiryStatusResponse, LivenessRequest, LivenessResponse,
    SubmitTxRequest, SubmitTxResponse,
};
use picomint_core::module::Method;
use picomint_core::{NumPeers, NumPeersExt, PeerId};
use picomint_encoding::Decodable;
use tokio::task::JoinSet;
use tracing::{debug, instrument, warn};

use crate::query::{QueryStep, QueryStrategy, ThresholdConsensus};
use crate::tx::{Transaction, TxError};

/// Federation API client.
///
/// Stateless: each request opens a fresh iroh [`Connection`] to the target
/// peer, sends one bi stream, then drops the connection. iroh's [`Endpoint`]
/// caches per-remote address + path state across calls (60s idle timeout),
/// so warm reconnects skip discovery and pay only the QUIC handshake.
#[derive(Clone, Debug)]
pub struct FederationApi {
    peer_node_ids: BTreeMap<PeerId, PublicKey>,
    endpoint: Endpoint,
}

impl FederationApi {
    pub fn new(endpoint: Endpoint, peer_node_ids: BTreeMap<PeerId, PublicKey>) -> Self {
        Self {
            peer_node_ids,
            endpoint,
        }
    }

    /// All federation peers.
    pub fn all_peers(&self) -> BTreeSet<PeerId> {
        self.peer_node_ids.keys().copied().collect()
    }

    /// Federation size, derived from the peer set.
    pub fn num_peers(&self) -> NumPeers {
        self.peer_node_ids.to_num_peers()
    }

    /// Iroh endpoint owned by this client. Re-used by module code that
    /// needs to talk to other iroh nodes (e.g. the Lightning module
    /// dialing gateways).
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    #[instrument(
        skip_all,
        fields(peer = %peer, method = ?method),
    )]
    pub async fn request_single_peer<R>(&self, method: Method, peer: PeerId) -> anyhow::Result<R>
    where
        R: Decodable,
    {
        let node_id = *self.peer_node_ids.get(&peer).context("Invalid peer id")?;

        picomint_rpc::request(&self.endpoint, node_id, method).await
    }

    /// As [`Self::request_single_peer`] but retries forever on transport /
    /// decode errors using `networking_backoff`. Used by the strategy-retry
    /// fan-out where every peer call must eventually yield a response.
    async fn request_single_peer_retry<R: Decodable>(&self, method: Method, peer: PeerId) -> R {
        (|| async {
            self.request_single_peer(method.clone(), peer)
                .await
                .inspect_err(|e| debug!(error = %e, "Peer request failed"))
        })
        .retry(networking_backoff())
        .await
        .expect("networking_backoff retries forever")
    }

    /// Make an aggregate request to federation, using `strategy` to logically
    /// merge the responses.
    #[instrument(skip_all, fields(method = ?method))]
    pub async fn request_with_strategy<P: Decodable + Send + 'static, F: Debug>(
        &self,
        mut strategy: impl QueryStrategy<P, F> + Send,
        method: Method,
    ) -> anyhow::Result<F> {
        let mut tasks = JoinSet::new();

        for peer in self.all_peers() {
            let api = self.clone();
            let method = method.clone();
            tasks.spawn(async move {
                let result = api.request_single_peer(method, peer).await;
                (peer, result)
            });
        }

        let mut peer_errors = BTreeMap::new();
        let peer_error_threshold = self.num_peers().one_honest();

        loop {
            let (peer, result) = tasks
                .join_next()
                .await
                .expect("Query strategy ran out of peers to query without returning a result")
                .expect("Per-peer request task panicked");

            match result {
                Ok(response) => match strategy.process(peer, response) {
                    QueryStep::Retry(peers) => {
                        for peer in peers {
                            let api = self.clone();
                            let method = method.clone();
                            tasks.spawn(async move {
                                let result = api.request_single_peer(method, peer).await;
                                (peer, result)
                            });
                        }
                    }
                    QueryStep::Success(response) => return Ok(response),
                    QueryStep::Failure(e) => {
                        peer_errors.insert(peer, e);
                    }
                    QueryStep::Continue => {}
                },
                Err(e) => {
                    debug!(error = %e, "Peer request failed");
                    peer_errors.insert(peer, e);
                }
            }

            if peer_errors.len() == peer_error_threshold {
                let err = anyhow!("Federation request {method:?} failed: {peer_errors:?}");
                warn!(err = %format_args!("{err:#}"), "federation request failed");
                return Err(err);
            }
        }
    }

    #[instrument(level = "debug", skip(self, strategy))]
    pub async fn request_with_strategy_retry<P: Decodable + Send + 'static, F: Debug>(
        &self,
        mut strategy: impl QueryStrategy<P, F> + Send,
        method: Method,
    ) -> F {
        let mut tasks = JoinSet::new();

        for peer in self.all_peers() {
            let api = self.clone();
            let method = method.clone();
            tasks.spawn(async move {
                let response = api.request_single_peer_retry(method, peer).await;
                (peer, response)
            });
        }

        loop {
            let (peer, response) = match tasks.join_next().await {
                Some(joined) => joined.expect("Per-peer request task panicked"),
                None => pending().await,
            };

            match strategy.process(peer, response) {
                QueryStep::Retry(peers) => {
                    for peer in peers {
                        let api = self.clone();
                        let method = method.clone();
                        tasks.spawn(async move {
                            let response = api.request_single_peer_retry(method, peer).await;
                            (peer, response)
                        });
                    }
                }
                QueryStep::Success(response) => return response,
                QueryStep::Failure(e) => {
                    warn!(error = %e, "Query strategy returned non-retryable failure");
                }
                QueryStep::Continue => {}
            }
        }
    }

    pub async fn request_current_consensus<R>(&self, method: Method) -> anyhow::Result<R>
    where
        R: Decodable + Eq + Debug + Clone + Send + 'static,
    {
        self.request_with_strategy(ThresholdConsensus::new(self.num_peers()), method)
            .await
    }

    pub async fn request_current_consensus_retry<R>(&self, method: Method) -> R
    where
        R: Decodable + Eq + Debug + Clone + Send + 'static,
    {
        self.request_with_strategy_retry(ThresholdConsensus::new(self.num_peers()), method)
            .await
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
