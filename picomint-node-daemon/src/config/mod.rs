use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::bail;
use bitcoin::Network;
use picomint_core::config::ConsensusConfig;
pub use picomint_core::config::{MintId, NodeEndpoint};
use picomint_core::ecash::config::{EcashConfig, EcashConfigPrivate};
use picomint_core::invite::InviteCode;
use picomint_core::lightning::config::LightningConfigPrivate;
use picomint_core::onchain::config::{OnchainConfig, OnchainConfigPrivate};
use picomint_core::version::CONSENSUS_VERSION;
use picomint_core::{NodeId, secp256k1};
use secp256k1::{Secp256k1, SecretKey, XOnlyPublicKey};
use serde::{Deserialize, Serialize};

use crate::config::setup::NodeSetupCode;
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
/// Full picomint node config (persisted in the node database).
pub struct NodeConfig {
    /// Mint-wide config, identical across nodes
    pub consensus: ConsensusConfig,
    /// Per-node secrets (identity + DKG keys)
    pub private: NodeConfigPrivate,
}

#[derive(Debug, Clone, Serialize, Deserialize, Encodable, Decodable)]
pub struct NodeConfigPrivate {
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

/// Process-level settings of the daemon, configured via env vars / args.
/// Live for the whole process — setup, DKG, and consensus alike.
#[derive(Clone)]
pub struct DaemonSettings {
    /// Bind address for our P2P connection
    pub p2p_addr: SocketAddr,
    /// Web UI bind address.
    pub ui_addr: SocketAddr,
    /// Bitcoin network for the mint
    pub network: Network,
    /// Path to the folder holding the database and the admin CLI socket
    pub data_dir: PathBuf,
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

impl NodeConfig {
    /// Assemble a fresh `NodeConfig` from the DKG parameters, the
    /// threshold-signing key pair we generated locally, and the per-module
    /// DKG outputs.
    pub fn from(
        params: DkgParams,
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

        let private = NodeConfigPrivate {
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
}

impl DkgParams {
    pub fn node_ids(&self) -> Vec<NodeId> {
        self.nodes.keys().copied().collect()
    }

    pub fn iroh_pks(&self) -> BTreeMap<NodeId, iroh_base::PublicKey> {
        self.nodes.iter().map(|(id, node)| (*id, node.pk)).collect()
    }
}
