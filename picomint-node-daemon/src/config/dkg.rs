use std::collections::BTreeMap;

use anyhow::Context;
use bls12_381::{G1Projective, G2Projective, Scalar};
use picomint_core::{NodeId, NumNodes};
use picomint_encoding::{Decodable, Encodable};
use secp256k1::{PublicKey, SecretKey};
use tracing::info;

use super::dkg_g1::run_dkg_g1;
use super::dkg_g2::run_dkg_g2;
use super::dkg_secp::run_dkg_secp;
use crate::p2p::{P2PMessage, Recipient, ReconnectP2PConnections};

/// A handle passed to DKG routines. Encapsulates the node id + p2p connection
/// machinery each module needs to run distributed key generation or exchange
/// arbitrary data with the other nodes.
#[non_exhaustive]
pub struct DkgHandle<'a> {
    #[doc(hidden)]
    pub num_nodes: NumNodes,
    #[doc(hidden)]
    pub identity: NodeId,
    #[doc(hidden)]
    pub connections: &'a ReconnectP2PConnections,
}

impl<'a> DkgHandle<'a> {
    pub fn new(
        num_nodes: NumNodes,
        identity: NodeId,
        connections: &'a ReconnectP2PConnections,
    ) -> Self {
        Self {
            num_nodes,
            identity,
            connections,
        }
    }

    pub fn num_nodes(&self) -> NumNodes {
        self.num_nodes
    }

    pub async fn run_dkg_g1(&self) -> anyhow::Result<(Vec<G1Projective>, Scalar)> {
        info!("Running distributed key generation for group G1...");

        run_dkg_g1(self.num_nodes, self.identity, self.connections).await
    }

    pub async fn run_dkg_g2(&self) -> anyhow::Result<(Vec<G2Projective>, Scalar)> {
        info!("Running distributed key generation for group G2...");

        run_dkg_g2(self.num_nodes, self.identity, self.connections).await
    }

    pub async fn run_dkg_secp(&self) -> anyhow::Result<(Vec<PublicKey>, SecretKey)> {
        info!("Running distributed key generation for secp256k1...");

        run_dkg_secp(self.num_nodes, self.identity, self.connections).await
    }

    /// Exchange a `P2PMessage::Encodable(Vec<u8>)` with all nodes. All nodes must
    /// be online and submit a response. The caller's message is included in
    /// the returned map under its own `NodeId`.
    pub async fn exchange_bytes(
        &self,
        bytes: Vec<u8>,
    ) -> anyhow::Result<BTreeMap<NodeId, Vec<u8>>> {
        info!("Exchanging raw bytes...");

        let mut node_data: BTreeMap<NodeId, Vec<u8>> = BTreeMap::new();

        self.connections
            .send(Recipient::Everyone, P2PMessage::Encodable(bytes.clone()));

        node_data.insert(self.identity, bytes);

        for node in self.num_nodes.node_ids().filter(|p| *p != self.identity) {
            let message = self
                .connections
                .receive_from_node(node)
                .await
                .context("Unexpected shutdown of p2p connections")?;

            match message {
                P2PMessage::Encodable(bytes) => {
                    node_data.insert(node, bytes);
                }
                message => {
                    anyhow::bail!("Invalid message from {node}: {message:?}");
                }
            }
        }

        Ok(node_data)
    }

    pub async fn exchange_encodable<T: Encodable + Decodable + Send + Sync>(
        &self,
        data: T,
    ) -> anyhow::Result<BTreeMap<NodeId, T>> {
        let mut decoded = BTreeMap::new();
        for (k, bytes) in self.exchange_bytes(data.consensus_encode_to_vec()).await? {
            decoded.insert(k, T::consensus_decode(&bytes)?);
        }
        Ok(decoded)
    }
}
