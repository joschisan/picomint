use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::mem;

use picomint_core::{NodeId, NumNodes};

/// Picomint query strategy
///
/// Due to federated security model each Picomint client API call to the
/// Mint might require a different way to process one or more required
/// responses from the Mint members. This trait abstracts away the details
/// of each specific strategy for the generic client Api code.
pub trait QueryStrategy<IR, OR = IR> {
    fn process(&mut self, node: NodeId, response: IR) -> QueryStep<OR>;
}

/// Results from the strategy handling a response from a node
///
/// Note that the implementation driving the [`QueryStrategy`] returning
/// [`QueryStep`] is responsible from remembering and collecting errors
/// for each node.
#[derive(Debug)]
pub enum QueryStep<R> {
    /// Retry requests to this nodes
    Retry(BTreeSet<NodeId>),
    /// Do nothing yet, keep waiting for requests
    Continue,
    /// Return the successful result
    Success(R),
    /// A non-retryable failure has occurred
    Failure(anyhow::Error),
}

/// Returns when we obtain a threshold of valid responses. RPC call errors or
/// invalid responses are not retried.
pub struct FilterMapThreshold<R, T> {
    filter_map: Box<dyn Fn(NodeId, R) -> anyhow::Result<T> + Send + Sync>,
    filtered_responses: BTreeMap<NodeId, T>,
    threshold: usize,
}

impl<R, T> FilterMapThreshold<R, T> {
    pub fn new(
        verifier: impl Fn(NodeId, R) -> anyhow::Result<T> + Send + Sync + 'static,
        num_nodes: NumNodes,
    ) -> Self {
        Self {
            filter_map: Box::new(verifier),
            filtered_responses: BTreeMap::new(),
            threshold: num_nodes.threshold(),
        }
    }
}

impl<R, T> QueryStrategy<R, BTreeMap<NodeId, T>> for FilterMapThreshold<R, T> {
    fn process(&mut self, node: NodeId, response: R) -> QueryStep<BTreeMap<NodeId, T>> {
        match (self.filter_map)(node, response) {
            Ok(response) => {
                self.filtered_responses.insert(node, response);

                if self.filtered_responses.len() == self.threshold {
                    QueryStep::Success(mem::take(&mut self.filtered_responses))
                } else {
                    QueryStep::Continue
                }
            }
            Err(e) => QueryStep::Failure(e),
        }
    }
}

/// Returns when we obtain a threshold of identical responses. Responses are not
/// assumed to be static and may be updated by the nodes; on failure to
/// establish consensus with a threshold of responses, we retry the requests.
/// RPC call errors are not retried.
pub struct ThresholdConsensus<R> {
    responses: BTreeMap<NodeId, R>,
    retry: BTreeSet<NodeId>,
    threshold: usize,
}

impl<R> ThresholdConsensus<R> {
    pub fn new(num_nodes: NumNodes) -> Self {
        Self {
            responses: BTreeMap::new(),
            retry: BTreeSet::new(),
            threshold: num_nodes.threshold(),
        }
    }
}

impl<R: Eq + Clone> QueryStrategy<R> for ThresholdConsensus<R> {
    fn process(&mut self, node: NodeId, response: R) -> QueryStep<R> {
        self.responses.insert(node, response.clone());

        if self.responses.values().filter(|r| **r == response).count() == self.threshold {
            return QueryStep::Success(response);
        }

        assert!(self.retry.insert(node));

        if self.retry.len() == self.threshold {
            QueryStep::Retry(mem::take(&mut self.retry))
        } else {
            QueryStep::Continue
        }
    }
}

#[test]
fn test_threshold_consensus() {
    let mut consensus = ThresholdConsensus::<u64>::new(NumNodes::from(4));

    assert!(matches!(
        consensus.process(NodeId::from(0), 1),
        QueryStep::Continue
    ));
    assert!(matches!(
        consensus.process(NodeId::from(1), 1),
        QueryStep::Continue
    ));
    assert!(matches!(
        consensus.process(NodeId::from(2), 0),
        QueryStep::Retry(..)
    ));

    assert!(matches!(
        consensus.process(NodeId::from(0), 1),
        QueryStep::Continue
    ));
    assert!(matches!(
        consensus.process(NodeId::from(1), 1),
        QueryStep::Continue
    ));
    assert!(matches!(
        consensus.process(NodeId::from(2), 1),
        QueryStep::Success(1)
    ));
}
