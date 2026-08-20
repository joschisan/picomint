use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::future::pending;

use anyhow::{Context, anyhow, bail};
use futures::StreamExt;
use futures::stream::BoxStream;
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
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_stream::wrappers::WatchStream;
use tracing::{debug, instrument};

use crate::connection::{ConnState, ConnStatus, connection_task, request_on_state};
use crate::query::{QueryStep, QueryStrategy, ThresholdConsensus};
use crate::tx::{Transaction, TxError};

/// Federation API client.
///
/// Spawns one background [`connection_task`] per peer at construction that
/// eagerly opens — and reconnects — a single kept-alive iroh connection,
/// publishing its [`ConnState`] on a watch channel. Every per-peer request
/// is multiplexed as a fresh bi stream over that pooled connection, so the
/// QUIC handshake and hole-punched path are paid once and reused, not per
/// request. Each task's status feeds [`Self::connection_status_stream`].
#[derive(Clone, Debug)]
pub struct FederationApi {
    peer_node_ids: BTreeMap<PeerId, PublicKey>,
    states: BTreeMap<PeerId, watch::Receiver<Option<ConnState>>>,
    endpoint: Endpoint,
}

impl FederationApi {
    pub fn new(endpoint: Endpoint, peer_node_ids: BTreeMap<PeerId, PublicKey>) -> Self {
        let mut states = BTreeMap::new();

        for (peer, node_id) in &peer_node_ids {
            let (tx, rx) = watch::channel(None);
            tokio::spawn(connection_task(*node_id, endpoint.clone(), tx));
            states.insert(*peer, rx);
        }

        Self {
            peer_node_ids,
            states,
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

    /// Stream of per-peer reachability. Emits a fresh `peer -> status` map
    /// whenever any peer's connection comes up or goes down, starting with the
    /// current state. Backed by the same kept-alive connections requests use,
    /// so it reflects real reachability, not a probe; the `Connected` status
    /// carries the RTT sampled at connect.
    pub fn connection_status_stream(&self) -> BoxStream<'static, BTreeMap<PeerId, ConnStatus>> {
        let streams = self.states.iter().map(|(&peer, rx)| {
            WatchStream::new(rx.clone()).map(move |s| {
                (
                    peer,
                    s.map_or(ConnStatus::Disconnected, |state| state.status()),
                )
            })
        });

        let mut current = BTreeMap::new();
        futures::stream::select_all(streams)
            .map(move |(peer, status)| {
                current.insert(peer, status);
                current.clone()
            })
            .boxed()
    }

    #[instrument(
        skip_all,
        fields(peer = %peer, method = ?method),
    )]
    pub async fn request_single_peer<R>(&self, method: Method, peer: PeerId) -> anyhow::Result<R>
    where
        R: Decodable,
    {
        let mut rx = self.states.get(&peer).context("Invalid peer id")?.clone();

        request_on_state(&mut rx, method).await
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
    ///
    /// A per-peer task that is cancelled rather than run to completion is
    /// dropped from the round: see [`handle_join_error`].
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
            let joined = match tasks.join_next().await {
                Some(joined) => joined,
                None => {
                    bail!("Federation request {method:?} failed: every peer task was cancelled")
                }
            };

            let (peer, result) = match joined {
                Ok(pair) => pair,
                Err(e) => match handle_join_error(e) {
                    Cancelled => continue,
                },
            };

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
                return Err(anyhow!(
                    "Federation request {method:?} failed: {peer_errors:?}"
                ));
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
                Some(Ok(pair)) => pair,
                Some(Err(e)) => match handle_join_error(e) {
                    Cancelled => continue,
                },
                // Every peer task has been consumed without the strategy
                // reaching a verdict. This layer retries forever by contract,
                // so there is nothing left to drive it — park instead of
                // spinning, and let the caller's own cancellation end it.
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
                    debug!(error = %e, "Query strategy returned non-retryable failure");
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

/// The one outcome of a [`JoinError`] this layer can carry on from.
struct Cancelled;

/// Split a [`JoinError`] into the two things it actually means.
///
/// A cancelled task is not a failure of the peer it was querying — the task
/// never got to say anything, and the error carries no peer id to attribute it
/// to, so the only honest thing is to drop it from the round. Tasks are
/// cancelled when the runtime they were spawned on winds down, which happens
/// routinely while a client is shutting down with requests still in flight.
///
/// A genuine panic is re-raised on this thread, preserving the original
/// payload and backtrace. Treating the two alike is what turned an ordinary
/// shutdown race into a process-killing `Per-peer request task panicked`.
fn handle_join_error(error: tokio::task::JoinError) -> Cancelled {
    if error.is_panic() {
        std::panic::resume_unwind(error.into_panic());
    }

    debug!(%error, "Per-peer request task was cancelled");

    Cancelled
}
