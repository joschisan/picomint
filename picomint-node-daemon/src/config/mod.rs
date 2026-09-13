use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::bail;
use bitcoin::Network;
pub use picomint_core::config::{MintId, NodeConfig, NodeEndpoint};
use picomint_core::config::{NodeConfigConsensus, NodeConfigPrivate};
use picomint_core::ecash::config::EcashConfig;
use picomint_core::onchain::config::OnchainConfig;
use picomint_core::version::CONSENSUS_VERSION;
use picomint_core::{NodeId, secp256k1};
use secp256k1::{Secp256k1, SecretKey, XOnlyPublicKey};

use crate::config::setup::NodeSetupCode;
use picomint_encoding::{Decodable, Encodable};

pub mod db;
pub mod dkg;
pub mod dkg_g1;
pub mod dkg_g2;
pub mod dkg_secp;
pub mod poly;
pub mod setup;

/// Process-level settings of the daemon, configured via env vars / args.
/// Live for the whole process — setup, DKG, and consensus alike.
#[derive(Clone)]
pub struct DaemonSettings {
    /// Bind address for our P2P connection
    pub p2p_addr: SocketAddr,
    /// Path to the folder holding the database and the admin CLI socket
    pub data_dir: PathBuf,
    /// Shortened polling intervals for the integration test.
    pub integration_test: bool,
}

/// Outcome of the setup phase: either fresh DKG params (run a DKG) or a
/// previously-backed-up `NodeConfig` to restore in place of one.
#[derive(Debug, Clone)]
pub enum SetupResult {
    Dkg(Box<DkgParams>),
    Restored(Box<NodeConfig>),
}

#[derive(Debug, Clone, Encodable, Decodable)]
/// Everything [`dkg::run`] needs to generate the `NodeConfig`: the output of
/// the setup phase, persisted so a daemon restart auto-resumes the DKG.
pub struct DkgParams {
    /// Our own node id
    pub identity: NodeId,
    /// Secret key for our single iroh endpoint (p2p + api)
    pub iroh_sk: iroh::SecretKey,
    /// Setup codes of all nodes
    pub nodes: BTreeMap<NodeId, NodeSetupCode>,
    /// Mint name, chosen by the leader during setup.
    pub name: String,
    /// Bitcoin network for this mint
    pub network: Network,
}

/// Assemble a fresh `NodeConfig` from the DKG parameters, the
/// threshold-signing key pair we generated locally, and the per-module
/// DKG outputs.
pub fn assemble_node_config(
    params: DkgParams,
    identity: NodeId,
    broadcast_public_keys: BTreeMap<NodeId, XOnlyPublicKey>,
    broadcast_secret_key: SecretKey,
    ecash: EcashConfig,
    lightning: picomint_core::lightning::config::LightningConfig,
    onchain: OnchainConfig,
) -> NodeConfig {
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

    let consensus = NodeConfigConsensus {
        nodes,
        network: params.network,
        name: params.name.clone(),
        default_version: CONSENSUS_VERSION,
        ecash: ecash.consensus,
        onchain: onchain.consensus,
        lightning: lightning.consensus,
    };

    let private = NodeConfigPrivate {
        identity,
        iroh_sk: params.iroh_sk,
        broadcast_secret_key,
        ecash: ecash.private,
        onchain: onchain.private,
        lightning: lightning.private,
    };

    NodeConfig { consensus, private }
}

/// The checks a config passes before the daemon runs on it: our broadcast
/// key is the one the node set lists for us, node ids are dense from 0,
/// and every module's keys fit its consensus config.
pub fn validate_config(cfg: &NodeConfig) -> anyhow::Result<()> {
    let nodes = &cfg.consensus.nodes;
    let my_public_key = cfg
        .private
        .broadcast_secret_key
        .public_key(&Secp256k1::new())
        .x_only_public_key()
        .0;

    if Some(my_public_key) != nodes.get(&cfg.private.identity).map(|p| p.broadcast_pk) {
        bail!("Broadcast secret key doesn't match corresponding public key");
    }
    if nodes.keys().max().copied().map(NodeId::to_usize) != Some(nodes.len() - 1) {
        bail!("Node ids are not indexed from 0");
    }
    if nodes.keys().min().copied() != Some(NodeId::from(0)) {
        bail!("Node ids are not indexed from 0");
    }

    crate::consensus::ecash::validate_config(cfg)?;
    crate::consensus::lightning::validate_config(cfg)?;
    crate::consensus::onchain::validate_config(cfg)?;

    Ok(())
}

impl DkgParams {
    pub fn node_ids(&self) -> Vec<NodeId> {
        self.nodes.keys().copied().collect()
    }

    pub fn iroh_pks(&self) -> BTreeMap<NodeId, iroh_base::PublicKey> {
        self.nodes.iter().map(|(id, node)| (*id, node.pk)).collect()
    }
}
