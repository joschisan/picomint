use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::future::pending;

use anyhow::{Context, anyhow};
use futures::StreamExt;
use futures::stream::BoxStream;
use iroh::endpoint::Connection;
use iroh::{Endpoint, PublicKey};
use picomint_core::backoff::{BackoffBuilder, Retryable, networking_backoff};
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
use tokio::time::sleep;
use tokio_stream::wrappers::WatchStream;
use tracing::{debug, instrument};

use crate::query::{QueryStep, QueryStrategy, ThresholdConsensus};
use crate::tx::{Transaction, TxError};

/// Live connection state for one peer, published on a watch channel by its
/// [`connection_task`]. `None` (the channel's initial value) means the task
/// has started but not yet produced a first result.
#[derive(Debug, Clone)]
enum PeerState {
    Connected(Connection),
    Disconnected,
}

/// Federation API client.
///
/// Spawns one background [`connection_task`] per peer at construction that
/// eagerly opens — and reconnects — a single kept-alive iroh connection,
/// publishing its [`PeerState`] on a watch channel. Every per-peer request
/// is multiplexed as a fresh bi stream over that pooled connection, so the
/// QUIC handshake and hole-punched path are paid once and reused, not per
/// request. Each task's status feeds [`Self::connection_status_stream`].
#[derive(Clone, Debug)]
pub struct FederationApi {
    peer_node_ids: BTreeMap<PeerId, PublicKey>,
    states: BTreeMap<PeerId, watch::Receiver<Option<PeerState>>>,
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

    /// Stream of per-peer reachability. Emits a fresh `peer -> connected`
    /// map whenever any peer's connection comes up or goes down, starting
    /// with the current state. Backed by the same kept-alive connections
    /// requests use, so it reflects real reachability, not a probe.
    pub fn connection_status_stream(&self) -> BoxStream<'static, BTreeMap<PeerId, bool>> {
        let streams = self.states.iter().map(|(&peer, rx)| {
            WatchStream::new(rx.clone())
                .map(move |s| (peer, matches!(s, Some(PeerState::Connected(_)))))
        });

        let mut current = BTreeMap::new();
        futures::stream::select_all(streams)
            .map(move |(peer, connected)| {
                current.insert(peer, connected);
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

        let state = rx
            .wait_for(Option::is_some)
            .await
            .expect("connection task outlives the api")
            .clone()
            .expect("wait_for guarantees Some");

        let PeerState::Connected(conn) = state else {
            return Err(anyhow!("Peer {peer} not connected"));
        };

        picomint_rpc::request_on_connection(&conn, method).await
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

/// Keep one iroh connection to `node_id` alive forever, publishing each
/// transition on `state`. Connect, announce `Connected`, block on
/// `Connection::closed`, announce `Disconnected`, then reconnect. Connect
/// failures back off via `networking_backoff` (reset on success); the loop
/// never terminates — it ends only when the watch receiver (i.e. the owning
/// `FederationApi`) is dropped, which makes `send_replace` a no-op the next
/// time around and the task is then cancelled with its runtime.
async fn connection_task(
    node_id: PublicKey,
    endpoint: Endpoint,
    state: watch::Sender<Option<PeerState>>,
) {
    let mut backoff = networking_backoff().build();

    loop {
        match endpoint.connect(node_id, picomint_rpc::ALPN).await {
            Ok(conn) => {
                backoff = networking_backoff().build();

                state.send_replace(Some(PeerState::Connected(conn.clone())));

                conn.closed().await;

                state.send_replace(Some(PeerState::Disconnected));
            }
            Err(_) => {
                sleep(backoff.next().expect("networking_backoff retries forever")).await;
            }
        }
    }
}
