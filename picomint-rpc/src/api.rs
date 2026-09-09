use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::future::pending;

use anyhow::{Context, anyhow};
use futures::StreamExt;
use futures::stream::{BoxStream, FuturesUnordered};
use iroh::{Endpoint, PublicKey};
use picomint_core::methods::Method;
use picomint_core::{NodeId, NumNodes, NumNodesExt};
use picomint_encoding::Decodable;
use tokio::sync::watch;
use tokio_stream::wrappers::WatchStream;
use tracing::{debug, instrument, warn};

use crate::connection::{
    ConnState, ConnStatus, connection_task, request_on_state, request_on_state_retry,
};
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

    /// The per-node request every fan-out is made of. Polled inline by the
    /// fan-out rather than spawned — a request is an await on the network,
    /// not work for another worker, and a future dropped with its fan-out
    /// simply ends.
    async fn request_node<P: Decodable>(
        &self,
        node: NodeId,
        method: Method,
    ) -> (NodeId, anyhow::Result<P>) {
        (node, request_on_state(&mut self.state(node), method).await)
    }

    /// As [`Self::request_node`] but never gives up on transport errors.
    async fn request_node_retry<P: Decodable>(&self, node: NodeId, method: Method) -> (NodeId, P) {
        let response = request_on_state_retry(&mut self.state(node), method)
            .await
            .expect("MintApi holds the receiver, so the connection task outlives us");

        (node, response)
    }

    /// Make an aggregate request to mint, using `strategy` to logically
    /// merge the responses.
    #[instrument(skip_all, fields(method = ?method))]
    pub async fn request_with_strategy<P: Decodable + Send + 'static, F: Debug>(
        &self,
        mut strategy: impl QueryStrategy<P, F> + Send,
        method: Method,
    ) -> anyhow::Result<F> {
        let mut requests: FuturesUnordered<_> = self
            .all_nodes()
            .into_iter()
            .map(|node| self.request_node(node, method.clone()))
            .collect();

        let mut node_errors = BTreeMap::new();
        let node_error_threshold = self.num_nodes().one_honest();

        loop {
            let (node, result) = requests
                .next()
                .await
                .expect("Query strategy ran out of nodes to query without returning a result");

            match result {
                Ok(response) => match strategy.process(node, response).await {
                    QueryStep::Retry(nodes) => {
                        for node in nodes {
                            requests.push(self.request_node(node, method.clone()));
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
        let mut requests: FuturesUnordered<_> = self
            .all_nodes()
            .into_iter()
            .map(|node| self.request_node_retry(node, method.clone()))
            .collect();

        loop {
            let (node, response) = match requests.next().await {
                Some(next) => next,
                None => pending().await,
            };

            match strategy.process(node, response).await {
                QueryStep::Retry(nodes) => {
                    for node in nodes {
                        requests.push(self.request_node_retry(node, method.clone()));
                    }
                }
                QueryStep::Success(response) => return response,
                QueryStep::Failure(e) => {
                    warn!(node = %node, error = %e, "Node response rejected by the query strategy");
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
