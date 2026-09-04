use std::collections::BTreeMap;
use std::iter::once;

use crate::config::poly::{g2, scalar};
use anyhow::{Context, bail, ensure};
use bls12_381::{G2Projective, Scalar};
use group::ff::Field;
use picomint_core::bitcoin::hashes::sha256;
use picomint_core::{NodeId, NumNodes};
use picomint_encoding::Encodable as _;
use rand::rngs::OsRng;
use tracing::trace;

use crate::p2p::{DkgMessageG2, P2PMessage, Recipient, ReconnectP2PConnections};

// Implementation of the classic Pedersen DKG for G2.

struct DkgG2 {
    num_nodes: NumNodes,
    identity: NodeId,
    polynomial: Vec<Scalar>,
    hash_commitments: BTreeMap<NodeId, sha256::Hash>,
    commitments: BTreeMap<NodeId, Vec<G2Projective>>,
    sk_shares: BTreeMap<NodeId, Scalar>,
}

impl DkgG2 {
    fn new(num_nodes: NumNodes, identity: NodeId) -> Self {
        let polynomial = (0..num_nodes.threshold())
            .map(|_| Scalar::random(&mut OsRng))
            .collect::<Vec<Scalar>>();

        let commitment = polynomial.iter().map(g2).collect::<Vec<G2Projective>>();

        DkgG2 {
            num_nodes,
            identity,
            polynomial,
            hash_commitments: once((identity, commitment.consensus_hash_sha256())).collect(),
            commitments: once((identity, commitment)).collect(),
            sk_shares: BTreeMap::new(),
        }
    }

    fn commitment(&self) -> Vec<G2Projective> {
        self.polynomial.iter().map(g2).collect()
    }

    fn initial_message(&self) -> DkgMessageG2 {
        DkgMessageG2::Hash(self.commitment().consensus_hash_sha256())
    }

    /// Runs a single step of the DKG algorithm
    fn step(&mut self, node: NodeId, msg: DkgMessageG2) -> anyhow::Result<DkgStepG2> {
        trace!(?node, ?msg, "Running DKG G2 step");
        match msg {
            DkgMessageG2::Hash(hash) => {
                ensure!(
                    self.hash_commitments.insert(node, hash).is_none(),
                    "DKG G2: node {node} sent us two hash commitments."
                );

                if self.hash_commitments.len() == self.num_nodes.total() {
                    return Ok(DkgStepG2::Broadcast(DkgMessageG2::Commitment(
                        self.commitment(),
                    )));
                }
            }
            DkgMessageG2::Commitment(polynomial) => {
                ensure!(
                    *self.hash_commitments.get(&node).with_context(|| format!(
                        "DKG G2: hash commitment not found for node {node}"
                    ))? == polynomial.consensus_hash_sha256(),
                    "DKG G2: polynomial commitment from node {node} is of wrong degree."
                );

                ensure!(
                    self.num_nodes.threshold() == polynomial.len(),
                    "DKG G2: polynomial commitment from node {node} is of wrong degree."
                );

                ensure!(
                    self.commitments.insert(node, polynomial).is_none(),
                    "DKG G2: node {node} sent us two commitments."
                );

                // Once everyone has send their commitments, send out the key shares...

                if self.commitments.len() == self.num_nodes.total() {
                    let mut messages = vec![];

                    for node in self.num_nodes.node_ids() {
                        let s = eval_poly_scalar(&self.polynomial, &scalar(&node));

                        if node == self.identity {
                            self.sk_shares.insert(self.identity, s);
                        } else {
                            messages.push((node, DkgMessageG2::Share(s)));
                        }
                    }

                    return Ok(DkgStepG2::Messages(messages));
                }
            }
            DkgMessageG2::Share(s) => {
                let polynomial = self.commitments.get(&node).with_context(|| {
                    format!("DKG G2: polynomial commitment not found for node {node}.")
                })?;

                let checksum: G2Projective = polynomial
                    .iter()
                    .zip((0..).map(|k| scalar(&self.identity).pow(&[k, 0, 0, 0])))
                    .map(|(c, x)| c * x)
                    .reduce(|a, b| a + b)
                    .expect("DKG G2: polynomial commitment from node is empty.");

                ensure!(g2(&s) == checksum, "DKG G2: share from {node} is invalid.");

                ensure!(
                    self.sk_shares.insert(node, s).is_none(),
                    "Node {node} sent us two sk shares."
                );

                if self.sk_shares.len() == self.num_nodes.total() {
                    let sks = self.sk_shares.values().sum();

                    let pks = (0..self.num_nodes.threshold())
                        .map(|i| {
                            self.commitments
                                .values()
                                .map(|coefficients| coefficients[i])
                                .reduce(|a, b| a + b)
                                .expect("DKG G2: polynomial commitments are empty.")
                        })
                        .collect();

                    return Ok(DkgStepG2::Result((pks, sks)));
                }
            }
        }

        Ok(DkgStepG2::Messages(vec![]))
    }
}

/// Runs the DKG G2 algorithm with our nodes. We do not handle any unexpected
/// messages and all nodes are expected to be cooperative.
pub async fn run_dkg_g2(
    num_nodes: NumNodes,
    identity: NodeId,
    connections: &ReconnectP2PConnections,
) -> anyhow::Result<(Vec<G2Projective>, Scalar)> {
    let mut dkg = DkgG2::new(num_nodes, identity);

    connections.send(
        Recipient::Everyone,
        P2PMessage::DkgG2(dkg.initial_message()),
    );

    loop {
        for node in num_nodes.node_ids().filter(|p| *p != identity) {
            let message = connections
                .receive_from_node(node)
                .await
                .context("Unexpected shutdown of p2p connections during dkg g2")?;

            let message = match message {
                P2PMessage::DkgG2(message) => message,
                _ => bail!("Received unexpected message during DKG G2: {message:?}"),
            };

            match dkg.step(node, message)? {
                DkgStepG2::Broadcast(message) => {
                    connections.send(Recipient::Everyone, P2PMessage::DkgG2(message));
                }
                DkgStepG2::Messages(messages) => {
                    for (node, message) in messages {
                        connections.send(Recipient::Node(node), P2PMessage::DkgG2(message));
                    }
                }
                DkgStepG2::Result(result) => {
                    return Ok(result);
                }
            }
        }
    }
}

fn eval_poly_scalar(coefficients: &[Scalar], x: &Scalar) -> Scalar {
    coefficients
        .iter()
        .copied()
        .rev()
        .reduce(|acc, coefficient| acc * x + coefficient)
        .expect("We have at least one coefficient")
}

enum DkgStepG2 {
    Broadcast(DkgMessageG2),
    Messages(Vec<(NodeId, DkgMessageG2)>),
    Result((Vec<G2Projective>, Scalar)),
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};

    use crate::config::poly::{eval_poly_g2, g2};
    use group::Curve;
    use picomint_core::{NodeId, NumNodes};

    use super::{DkgG2, DkgStepG2};

    #[test_log::test]
    fn test_dkg_g2() {
        let num_nodes = NumNodes::from(7);

        let mut dkgs = num_nodes
            .node_ids()
            .map(|node| (node, DkgG2::new(num_nodes, node)))
            .collect::<BTreeMap<NodeId, DkgG2>>();

        let mut steps = dkgs
            .iter()
            .map(|(node, dkg)| (*node, DkgStepG2::Broadcast(dkg.initial_message())))
            .collect::<VecDeque<(NodeId, DkgStepG2)>>();

        let mut keys = BTreeMap::new();

        while keys.len() < num_nodes.total() {
            match steps.pop_front().unwrap() {
                (send_node, DkgStepG2::Broadcast(message)) => {
                    for receive_node in num_nodes.node_ids().filter(|p| *p != send_node) {
                        let step = dkgs
                            .get_mut(&receive_node)
                            .unwrap()
                            .step(send_node, message.clone());

                        steps.push_back((receive_node, step.unwrap()));
                    }
                }
                (send_node, DkgStepG2::Messages(messages)) => {
                    for (receive_node, message) in messages {
                        let step = dkgs
                            .get_mut(&receive_node)
                            .unwrap()
                            .step(send_node, message);

                        steps.push_back((receive_node, step.unwrap()));
                    }
                }
                (send_node, DkgStepG2::Result(step_keys)) => {
                    keys.insert(send_node, step_keys);
                }
            }
        }

        assert!(steps.is_empty());

        for (node, (poly_g2, sks)) in keys {
            assert_eq!(poly_g2.len(), 5);
            assert_eq!(eval_poly_g2(&poly_g2, &node), g2(&sks).to_affine());
        }
    }
}
