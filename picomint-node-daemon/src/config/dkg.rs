use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::{Context, bail};
use bls12_381::{G1Projective, G2Projective, Scalar};
use picomint_core::{NodeId, NumNodes, NumNodesExt, secp256k1};
use picomint_encoding::{Decodable, Encodable};
use rand::rngs::OsRng;
use secp256k1::{PublicKey, SecretKey};
use tokio::time::sleep;
use tracing::{error, info, warn};

use super::dkg_g1::run_dkg_g1;
use super::dkg_g2::run_dkg_g2;
use super::dkg_secp::run_dkg_secp;
use crate::config::{DkgParams, NodeConfig};
use crate::p2p::{P2PMessage, P2PStatusReceivers, Recipient, ReconnectP2PConnections};

/// Runs the DKG: waits for all p2p connections, cross-checks the setup
/// codes, generates the per-module keys, and cross-checks the resulting
/// consensus config.
pub async fn run(
    params: &DkgParams,
    connections: ReconnectP2PConnections,
    p2p_status_receivers: P2PStatusReceivers,
) -> anyhow::Result<NodeConfig> {
    info!("Waiting for all p2p connections to open...");

    loop {
        let disconnected_nodes: BTreeSet<NodeId> = p2p_status_receivers
            .iter()
            .filter_map(|(p, r)| r.borrow().is_disconnected().then_some(*p))
            .collect();

        if disconnected_nodes.is_empty() {
            break;
        }

        info!(
            pending = ?disconnected_nodes,
            "Waiting for all p2p connections to open..."
        );

        sleep(Duration::from_secs(3)).await;
    }

    let checksum = params.nodes.consensus_hash_sha256();

    info!("Comparing setup codes checksum {checksum}...");

    connections.send(Recipient::Everyone, P2PMessage::Checksum(checksum));

    for node in params
        .node_ids()
        .into_iter()
        .filter(|p| *p != params.identity)
    {
        let node_message = connections
            .receive_from_node(node)
            .await
            .context("Unexpected shutdown of p2p connections")?;

        if node_message != P2PMessage::Checksum(checksum) {
            error!(
                expected = ?P2PMessage::Checksum(checksum),
                received = ?node_message,
                "Node {node} has sent invalid setup codes checksum message"
            );

            bail!("Node {node} has sent invalid setup codes checksum message");
        }

        info!("Node {node} has sent valid setup codes checksum message");
    }

    let handle = DkgHandle::new(params.nodes.to_num_nodes(), params.identity, &connections);

    let (broadcast_sk, broadcast_pk) = secp256k1::generate_keypair(&mut OsRng);
    let broadcast_pk = broadcast_pk.x_only_public_key().0;

    let broadcast_public_keys = handle.exchange_encodable(broadcast_pk).await?;

    info!("Running DKG for the ecash module...");

    let ecash = crate::consensus::ecash::dkg(&handle).await?;

    info!("Running DKG for the lightning module...");

    let lightning = crate::consensus::lightning::dkg(&handle).await?;

    info!("Running DKG for the onchain module...");

    let onchain = crate::consensus::onchain::dkg(&handle).await?;

    let cfg = NodeConfig::from(
        params.clone(),
        params.identity,
        broadcast_public_keys,
        broadcast_sk,
        ecash,
        lightning,
        onchain,
    );

    let checksum = cfg.consensus.consensus_hash_sha256();

    info!("Comparing consensus config checksum {checksum}...");

    connections.send(Recipient::Everyone, P2PMessage::Checksum(checksum));

    for node in params
        .node_ids()
        .into_iter()
        .filter(|p| *p != params.identity)
    {
        let node_message = connections
            .receive_from_node(node)
            .await
            .context("Unexpected shutdown of p2p connections")?;

        if node_message != P2PMessage::Checksum(checksum) {
            warn!(
                expected = ?P2PMessage::Checksum(checksum),
                received = ?node_message,
                config = ?cfg.consensus,
                "Node {node} has sent invalid consensus config checksum message"
            );

            bail!("Node {node} has sent invalid consensus config checksum message");
        }

        info!("Node {node} has sent valid consensus config checksum message");
    }

    info!("DKG has completed successfully!");

    Ok(cfg)
}

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
