use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::future::pending;
use std::time::Duration;

use anyhow::{Context, anyhow};
use futures::StreamExt;
use futures::stream::BoxStream;
use iroh::{Endpoint, PublicKey};
use picomint_core::backoff::{Retryable, networking_backoff};
use picomint_core::module::Method;
use picomint_core::{NodeId, NumNodes, NumNodesExt};
use picomint_encoding::Decodable;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep_until};
use tokio_stream::wrappers::WatchStream;
use tracing::{debug, instrument, warn};

/// A mint request still unresolved after this long is logged once at
/// warn, with what it has heard so far, so a silent hang has a signature.
const PENDING_WARN_AFTER: Duration = Duration::from_secs(10);

/// Run one node's request and log how it went at debug.
async fn timed_request<R: Decodable>(
    node: NodeId,
    mut rx: watch::Receiver<Option<ConnState>>,
    method: Method,
) -> (NodeId, anyhow::Result<R>) {
    let name = method.name();
    let start = Instant::now();
    let result = request_on_state(&mut rx, method).await;

    debug!(
        %node,
        method = name,
        elapsed_ms = start.elapsed().as_millis() as u64,
        ok = result.is_ok(),
        "node request finished"
    );

    (node, result)
}

use crate::connection::{ConnState, ConnStatus, connection_task, request_on_state};
use crate::query::{QueryStep, QueryStrategy, ThresholdConsensus};

/// Mint API client: a pool of kept-alive connections to a mint's
/// nodes, with the query strategies for fanning a request across them.
///
/// Spans the whole mint — [`Self::request_with_strategy`] gives up once
/// `f + 1` nodes have errored, so the node set must have a mint's shape.
/// A one-shot request to some subset of nodes wants [`crate::request`]
/// instead, which pays for a connection it does not keep.
///
/// Spawns one background [`connection_task`] per node at construction that
/// eagerly opens — and reconnects — a single kept-alive iroh connection,
/// publishing its [`ConnState`] on a watch channel. Every per-node request
/// is multiplexed as a fresh bi stream over that pooled connection, so the
/// QUIC handshake and hole-punched path are paid once and reused, not per
/// request. Each task's status feeds [`Self::connection_status_stream`].
#[derive(Clone, Debug)]
pub struct MintApi {
    nodes: BTreeMap<NodeId, PublicKey>,
    states: BTreeMap<NodeId, watch::Receiver<Option<ConnState>>>,
}

impl MintApi {
    pub fn new(endpoint: Endpoint, nodes: BTreeMap<NodeId, PublicKey>) -> Self {
        let mut states = BTreeMap::new();

        for (node, iroh_pk) in &nodes {
            let (tx, rx) = watch::channel(None);
            tokio::spawn(connection_task(*iroh_pk, endpoint.clone(), tx));
            states.insert(*node, rx);
        }

        Self { nodes, states }
    }

    /// Every node in the pool.
    pub fn all_nodes(&self) -> BTreeSet<NodeId> {
        self.nodes.keys().copied().collect()
    }

    /// Mint size, derived from the node set. Panics unless the pool
    /// spans a whole mint — a subset has no such shape.
    pub fn num_nodes(&self) -> NumNodes {
        self.nodes.to_num_nodes()
    }

    /// Stream of per-node reachability. Emits a fresh `node -> status` map
    /// whenever any node's connection comes up or goes down, starting with the
    /// current state. Backed by the same kept-alive connections requests use,
    /// so it reflects real reachability, not a probe; the `Connected` status
    /// carries the RTT sampled at connect.
    pub fn connection_status_stream(&self) -> BoxStream<'static, BTreeMap<NodeId, ConnStatus>> {
        let streams = self.states.iter().map(|(&node, rx)| {
            WatchStream::new(rx.clone()).map(move |s| {
                (
                    node,
                    s.map_or(ConnStatus::Disconnected, |state| state.status()),
                )
            })
        });

        let mut current = BTreeMap::new();
        futures::stream::select_all(streams)
            .map(move |(node, status)| {
                current.insert(node, status);
                current.clone()
            })
            .boxed()
    }

    fn state(&self, node: NodeId) -> watch::Receiver<Option<ConnState>> {
        self.states
            .get(&node)
            .expect("Strategies only retry nodes from the pool")
            .clone()
    }

    #[instrument(
        skip_all,
        fields(node = %node, method = ?method),
    )]
    pub async fn request_single_node<R>(&self, method: Method, node: NodeId) -> anyhow::Result<R>
    where
        R: Decodable,
    {
        let mut rx = self.states.get(&node).context("Invalid node id")?.clone();

        request_on_state(&mut rx, method).await
    }

    /// Make an aggregate request to mint, using `strategy` to logically
    /// merge the responses.
    #[instrument(skip_all, fields(method = ?method))]
    pub async fn request_with_strategy<P: Decodable + Send + 'static, F: Debug>(
        &self,
        mut strategy: impl QueryStrategy<P, F> + Send,
        method: Method,
    ) -> anyhow::Result<F> {
        let mut tasks = JoinSet::new();

        for (node, rx) in self.states.clone() {
            tasks.spawn(timed_request(node, rx, method.clone()));
        }

        let mut node_errors = BTreeMap::new();
        let node_error_threshold = self.num_nodes().one_honest();

        let start = Instant::now();
        let mut answered = 0usize;
        let mut warned = false;

        loop {
            let joined = tokio::select! {
                joined = tasks.join_next() => joined,
                () = sleep_until(start + PENDING_WARN_AFTER), if !warned => {
                    warned = true;
                    warn!(
                        method = method.name(),
                        answered,
                        errors = node_errors.len(),
                        "mint request still pending after {PENDING_WARN_AFTER:?}"
                    );
                    continue;
                }
            };

            let (node, result) = joined
                .expect("Query strategy ran out of nodes to query without returning a result")
                .expect("Per-node request task panicked");

            answered += 1;

            match result {
                Ok(response) => match strategy.process(node, response) {
                    QueryStep::Retry(nodes) => {
                        for node in nodes {
                            tasks.spawn(timed_request(node, self.state(node), method.clone()));
                        }
                    }
                    QueryStep::Success(response) => return Ok(response),
                    QueryStep::Failure(e) => {
                        node_errors.insert(node, e);
                    }
                    QueryStep::Continue => {}
                },
                Err(e) => {
                    debug!(error = %e, "Node request failed");
                    node_errors.insert(node, e);
                }
            }

            if node_errors.len() == node_error_threshold {
                return Err(anyhow!("Mint request {method:?} failed: {node_errors:?}"));
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

        for (node, rx) in self.states.clone() {
            let method = method.clone();
            tasks.spawn(async move {
                let response = request_on_state_retry(rx, method).await;
                (node, response)
            });
        }

        let start = Instant::now();
        let mut answered = 0usize;
        let mut warned = false;

        loop {
            let joined = tokio::select! {
                joined = tasks.join_next() => joined,
                () = sleep_until(start + PENDING_WARN_AFTER), if !warned => {
                    warned = true;
                    warn!(
                        method = method.name(),
                        answered,
                        "mint request still pending after {PENDING_WARN_AFTER:?}"
                    );
                    continue;
                }
            };

            let (node, response) = match joined {
                Some(joined) => joined.expect("Per-node request task panicked"),
                None => pending().await,
            };

            answered += 1;

            match strategy.process(node, response) {
                QueryStep::Retry(nodes) => {
                    for node in nodes {
                        let rx = self.state(node);
                        let method = method.clone();
                        tasks.spawn(async move {
                            let response = request_on_state_retry(rx, method).await;
                            (node, response)
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
        self.request_with_strategy(ThresholdConsensus::new(self.num_nodes()), method)
            .await
    }

    pub async fn request_current_consensus_retry<R>(&self, method: Method) -> R
    where
        R: Decodable + Eq + Debug + Clone + Send + 'static,
    {
        self.request_with_strategy_retry(ThresholdConsensus::new(self.num_nodes()), method)
            .await
    }
}

/// As [`request_on_state`] but retries forever on transport / decode errors
/// using `networking_backoff`. Used by the strategy-retry fan-out where
/// every node call must eventually yield a response.
async fn request_on_state_retry<R: Decodable>(
    rx: watch::Receiver<Option<ConnState>>,
    method: Method,
) -> R {
    (|| async {
        request_on_state(&mut rx.clone(), method.clone())
            .await
            .inspect_err(|e| debug!(error = %e, "Node request failed"))
    })
    .retry(networking_backoff())
    .await
    .expect("networking_backoff retries forever")
}
