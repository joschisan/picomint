use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};
use dkg::DkgHandle;
use picomint_core::config::ConsensusConfig;
pub use picomint_core::config::{MintId, NodeEndpoint};
use picomint_core::ecash::config::{EcashConfig, EcashConfigPrivate};
use picomint_core::invite::InviteCode;
use picomint_core::lightning::config::LightningConfigPrivate;
use picomint_core::onchain::config::{OnchainConfig, OnchainConfigPrivate};
use picomint_core::version::CONSENSUS_VERSION;
use picomint_core::{NodeId, NumNodesExt, secp256k1};
use rand::rngs::OsRng;
use secp256k1::{Secp256k1, SecretKey, XOnlyPublicKey};
use serde::{Deserialize, Serialize};
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::config::setup::NodeSetupCode;
use crate::p2p::{P2PMessage, P2PStatusReceivers, Recipient, ReconnectP2PConnections};
use picomint_encoding::{Decodable, Encodable};

pub mod db;
pub mod dkg;
pub mod dkg_g1;
pub mod dkg_g2;
pub mod dkg_secp;
pub mod poly;
pub mod setup;

#[allow(clippy::unsafe_derive_deserialize)] // clippy fires on `select!` https://github.com/rust-lang/rust-clippy/issues/13062
#[derive(Debug, Clone, Serialize, Deserialize, Encodable, Decodable)]
/// Full picomint server config (persisted in the node database).
pub struct ServerConfig {
    /// Mint-wide config, identical across nodes
    pub consensus: ConsensusConfig,
    /// Per-node secrets (identity + DKG keys)
    pub private: ServerConfigPrivate,
}

#[derive(Debug, Clone, Serialize, Deserialize, Encodable, Decodable)]
pub struct ServerConfigPrivate {
    /// Our node id
    pub identity: NodeId,
    /// Secret key for our single iroh endpoint (p2p + api)
    pub iroh_sk: iroh::SecretKey,
    /// Secret key for the atomic broadcast to sign messages
    pub broadcast_secret_key: SecretKey,
    /// Private key material for the ecash module
    pub ecash: EcashConfigPrivate,
    /// Private key material for the onchain module
    pub onchain: OnchainConfigPrivate,
    /// Private key material for the lightning module
    pub lightning: LightningConfigPrivate,
}

/// All the info we configure prior to config gen starting
#[derive(Clone)]
pub struct ConfigGenSettings {
    /// Bind address for our P2P connection
    pub p2p_addr: SocketAddr,
    /// Web UI bind address.
    pub ui_addr: SocketAddr,
    /// Bitcoin network for the mint
    pub network: bitcoin::Network,
    /// Path to the folder holding the database and the admin CLI socket
    pub data_dir: PathBuf,
}

/// Outcome of the setup phase: either fresh DKG params (run a DKG) or a
/// previously-backed-up `ServerConfig` to restore in place of one.
#[derive(Debug, Clone)]
pub enum SetupResult {
    Dkg(Box<ConfigGenParams>),
    Restored(Box<ServerConfig>),
}

#[derive(Debug, Clone, Encodable, Decodable)]
/// All the parameters necessary for generating the `ServerConfig` during setup
///
/// * Nodes can create the parameters using a setup UI or CLI tool
/// * Used for distributed config generation
pub struct ConfigGenParams {
    /// Our own node id
    pub identity: NodeId,
    /// Secret key for our single iroh endpoint (p2p + api)
    pub iroh_sk: iroh::SecretKey,
    /// Endpoints of all servers
    pub nodes: BTreeMap<NodeId, NodeSetupCode>,
    /// Mint name, chosen by the lead node during setup.
    pub name: String,
    /// Bitcoin network for this mint
    pub network: bitcoin::Network,
}

impl ServerConfig {
    /// Assemble a fresh `ServerConfig` from config-gen parameters, the
    /// threshold-signing key pair we generated locally, and the per-module
    /// DKG outputs.
    pub fn from(
        params: ConfigGenParams,
        identity: NodeId,
        broadcast_public_keys: BTreeMap<NodeId, XOnlyPublicKey>,
        broadcast_secret_key: SecretKey,
        ecash: EcashConfig,
        lightning: picomint_core::lightning::config::LightningConfig,
        onchain: OnchainConfig,
    ) -> Self {
        let nodes = params
            .nodes
            .iter()
            .map(|(id, node)| {
                let endpoint = NodeEndpoint {
                    iroh_pk: node.pk,
                    broadcast_pk: *broadcast_public_keys
                        .get(id)
                        .expect("broadcast pk for every node"),
                    name: node.name.clone(),
                };
                (*id, endpoint)
            })
            .collect();

        let consensus = ConsensusConfig {
            nodes,
            network: params.network,
            name: params.name.clone(),
            default_version: CONSENSUS_VERSION,
            ecash: ecash.consensus,
            onchain: onchain.consensus,
            lightning: lightning.consensus,
        };

        let private = ServerConfigPrivate {
            identity,
            iroh_sk: params.iroh_sk,
            broadcast_secret_key,
            ecash: ecash.private,
            onchain: onchain.private,
            lightning: lightning.private,
        };

        Self { consensus, private }
    }

    pub fn get_invite_code(&self, invite_id: [u8; 16]) -> InviteCode {
        InviteCode::new(
            self.private.iroh_sk.public(),
            self.consensus.calculate_mint_id(),
            invite_id,
        )
    }

    pub fn validate_config(&self) -> anyhow::Result<()> {
        let nodes = &self.consensus.nodes;
        let my_public_key = self
            .private
            .broadcast_secret_key
            .public_key(&Secp256k1::new())
            .x_only_public_key()
            .0;

        if Some(my_public_key) != nodes.get(&self.private.identity).map(|p| p.broadcast_pk) {
            bail!("Broadcast secret key doesn't match corresponding public key");
        }
        if nodes.keys().max().copied().map(NodeId::to_usize) != Some(nodes.len() - 1) {
            bail!("Node ids are not indexed from 0");
        }
        if nodes.keys().min().copied() != Some(NodeId::from(0)) {
            bail!("Node ids are not indexed from 0");
        }

        crate::consensus::ecash::validate_config(self)?;
        crate::consensus::lightning::validate_config(self)?;
        crate::consensus::onchain::validate_config(self)?;

        Ok(())
    }

    /// Runs the distributed key gen algorithm
    pub async fn generate(
        params: &ConfigGenParams,
        connections: ReconnectP2PConnections,
        p2p_status_receivers: P2PStatusReceivers,
    ) -> anyhow::Result<Self> {
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

        info!("Comparing connection codes checksum {checksum}...");

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
                    "Node {node} has sent invalid connection code checksum message"
                );

                bail!("Node {node} has sent invalid connection code checksum message");
            }

            info!("Node {node} has sent valid connection code checksum message");
        }

        info!("Running config generation...");

        let handle = DkgHandle::new(params.nodes.to_num_nodes(), params.identity, &connections);

        let (broadcast_sk, broadcast_pk) = secp256k1::generate_keypair(&mut OsRng);
        let broadcast_pk = broadcast_pk.x_only_public_key().0;

        let broadcast_public_keys = handle.exchange_encodable(broadcast_pk).await?;

        info!("Running config generation for module of kind ecash...");

        let ecash = crate::consensus::ecash::distributed_gen(&handle).await?;

        info!("Running config generation for module of kind lightning...");

        let lightning = crate::consensus::lightning::distributed_gen(&handle).await?;

        info!("Running config generation for module of kind onchain...");

        let onchain = crate::consensus::onchain::distributed_gen(&handle).await?;

        let cfg = ServerConfig::from(
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

        info!("Config generation has completed successfully!");

        Ok(cfg)
    }
}

impl ConfigGenParams {
    pub fn node_ids(&self) -> Vec<NodeId> {
        self.nodes.keys().copied().collect()
    }

    pub fn iroh_pks(&self) -> BTreeMap<NodeId, iroh_base::PublicKey> {
        self.nodes.iter().map(|(id, node)| (*id, node.pk)).collect()
    }
}
