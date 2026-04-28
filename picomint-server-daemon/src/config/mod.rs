use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, bail};
use bitcoin::Network;
use dkg::DkgHandle;
use futures::future::select_all;
use picomint_core::config::ConsensusConfig;
pub use picomint_core::config::{FederationId, PeerEndpoint};
use picomint_core::invite_code::InviteCode;
use picomint_core::ln::config::LightningConfigPrivate;
use picomint_core::mint::config::{MintConfig, MintConfigPrivate};
use picomint_core::module::ApiAuth;
use picomint_core::wallet::config::{WalletConfig, WalletConfigPrivate};
use picomint_core::{NumPeersExt, PeerId, secp256k1};
use picomint_logging::LOG_NET_PEER_DKG;
use rand::rngs::OsRng;
use secp256k1::{PublicKey, Secp256k1, SecretKey};
use serde::{Deserialize, Serialize};
use tokio::select;
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::config::setup::PeerSetupCode;
use crate::p2p::{P2PMessage, P2PStatusReceivers, Recipient, ReconnectP2PConnections};
use picomint_encoding::{Decodable, Encodable};

pub mod db;
pub mod dkg;
pub mod dkg_g1;
pub mod dkg_g2;
pub mod poly;
pub mod setup;

/// How many concurrent Iroh API connections the server will accept.
pub const MAX_CLIENT_CONNECTIONS: u32 = 1000;

/// AlephBFT rounds per session. Controls session duration (3 min prod / 10 s
/// regtest).
const DEFAULT_ALEPH_ROUNDS_PER_SESSION: u16 = 3600;
const REGTEST_ALEPH_ROUNDS_PER_SESSION: u16 = 200;

fn aleph_rounds_per_session(network: Network) -> u16 {
    if network == Network::Regtest {
        REGTEST_ALEPH_ROUNDS_PER_SESSION
    } else {
        DEFAULT_ALEPH_ROUNDS_PER_SESSION
    }
}

#[allow(clippy::unsafe_derive_deserialize)] // clippy fires on `select!` https://github.com/rust-lang/rust-clippy/issues/13062
#[derive(Debug, Clone, Serialize, Deserialize, Encodable, Decodable)]
/// Full picomint server config (persisted in redb).
pub struct ServerConfig {
    /// Federation-wide config, identical across peers
    pub consensus: ConsensusConfig,
    /// Per-peer secrets (identity + DKG keys)
    pub private: ServerConfigPrivate,
}

#[derive(Debug, Clone, Serialize, Deserialize, Encodable, Decodable)]
pub struct ServerConfigPrivate {
    /// Our peer id
    pub identity: PeerId,
    /// Secret key for our single iroh endpoint (p2p + api)
    pub iroh_sk: iroh::SecretKey,
    /// Secret key for the atomic broadcast to sign messages
    pub broadcast_secret_key: SecretKey,
    /// Private key material for the mint module
    pub mint: MintConfigPrivate,
    /// Private key material for the wallet module
    pub wallet: WalletConfigPrivate,
    /// Private key material for the lightning module
    pub ln: LightningConfigPrivate,
}

/// All the info we configure prior to config gen starting
#[derive(Clone)]
pub struct ConfigGenSettings {
    /// Bind address for our P2P connection
    pub p2p_addr: SocketAddr,
    /// Web UI bind address + admin password. `None` disables the UI and
    /// requires all admin actions (including DKG setup) to go through the
    /// CLI. `main` rejects boot if `UI_ADDR` is set without `UI_PASSWORD`
    /// or vice versa, so these are always populated together.
    pub ui_config: Option<(SocketAddr, ApiAuth)>,
    /// Bitcoin network for the federation
    pub network: bitcoin::Network,
}

/// Outcome of the setup phase: either fresh DKG params (run a DKG) or a
/// previously-backed-up `ServerConfig` to restore in place of one.
#[derive(Debug, Clone)]
pub enum SetupResult {
    Dkg(Box<ConfigGenParams>),
    Restored(Box<ServerConfig>),
}

#[derive(Debug, Clone)]
/// All the parameters necessary for generating the `ServerConfig` during setup
///
/// * Guardians can create the parameters using a setup UI or CLI tool
/// * Used for distributed or trusted config generation
pub struct ConfigGenParams {
    /// Our own peer id
    pub identity: PeerId,
    /// Secret key for our single iroh endpoint (p2p + api)
    pub iroh_sk: iroh::SecretKey,
    /// Endpoints of all servers
    pub peers: BTreeMap<PeerId, PeerSetupCode>,
    /// Federation name, chosen by the lead guardian during setup.
    pub name: String,
    /// Bitcoin network for this federation
    pub network: bitcoin::Network,
}

impl ServerConfig {
    /// Assemble a fresh `ServerConfig` from config-gen parameters, the
    /// threshold-signing key pair we generated locally, and the per-module
    /// DKG outputs.
    pub fn from(
        params: ConfigGenParams,
        identity: PeerId,
        broadcast_public_keys: BTreeMap<PeerId, PublicKey>,
        broadcast_secret_key: SecretKey,
        mint: MintConfig,
        ln: picomint_core::ln::config::LightningConfig,
        wallet: WalletConfig,
    ) -> Self {
        let peers = params
            .peers
            .iter()
            .map(|(id, peer)| {
                let endpoint = PeerEndpoint {
                    iroh_pk: peer.pk,
                    broadcast_pk: *broadcast_public_keys
                        .get(id)
                        .expect("broadcast pk for every peer"),
                    name: peer.name.clone(),
                };
                (*id, endpoint)
            })
            .collect();

        let consensus = ConsensusConfig {
            peers,
            aleph_rounds_per_session: aleph_rounds_per_session(params.network),
            network: params.network,
            name: params.name.clone(),
            mint: mint.consensus,
            wallet: wallet.consensus,
            ln: ln.consensus,
        };

        let private = ServerConfigPrivate {
            identity,
            iroh_sk: params.iroh_sk,
            broadcast_secret_key,
            mint: mint.private,
            wallet: wallet.private,
            ln: ln.private,
        };

        Self { consensus, private }
    }

    pub fn get_invite_code(&self) -> InviteCode {
        InviteCode::new(
            self.private.iroh_sk.public(),
            self.private.identity,
            self.consensus.calculate_federation_id(),
        )
    }

    /// Bundle the current peer's typed configs back into per-module
    /// `*Config` values for passing into the module constructors.
    pub fn mint_config(&self) -> MintConfig {
        MintConfig {
            private: self.private.mint.clone(),
            consensus: self.consensus.mint.clone(),
        }
    }

    pub fn ln_config(&self) -> picomint_core::ln::config::LightningConfig {
        picomint_core::ln::config::LightningConfig {
            private: self.private.ln.clone(),
            consensus: self.consensus.ln.clone(),
        }
    }

    pub fn wallet_config(&self) -> WalletConfig {
        WalletConfig {
            private: self.private.wallet.clone(),
            consensus: self.consensus.wallet.clone(),
        }
    }

    pub fn validate_config(&self, identity: &PeerId) -> anyhow::Result<()> {
        let peers = &self.consensus.peers;
        let my_public_key = self
            .private
            .broadcast_secret_key
            .public_key(&Secp256k1::new());

        if Some(my_public_key) != peers.get(identity).map(|p| p.broadcast_pk) {
            bail!("Broadcast secret key doesn't match corresponding public key");
        }
        if peers.keys().max().copied().map(PeerId::to_usize) != Some(peers.len() - 1) {
            bail!("Peer ids are not indexed from 0");
        }
        if peers.keys().min().copied() != Some(PeerId::from(0)) {
            bail!("Peer ids are not indexed from 0");
        }

        crate::consensus::mint::validate_config(identity, &self.mint_config())?;
        crate::consensus::ln::validate_config(identity, &self.ln_config())?;
        crate::consensus::wallet::validate_config(identity, &self.wallet_config())?;

        Ok(())
    }

    /// Runs the distributed key gen algorithm
    pub async fn distributed_gen(
        params: &ConfigGenParams,
        connections: ReconnectP2PConnections<P2PMessage>,
        mut p2p_status_receivers: P2PStatusReceivers,
    ) -> anyhow::Result<Self> {
        info!(
            target: LOG_NET_PEER_DKG,
            "Waiting for all p2p connections to open..."
        );

        loop {
            let mut pending_connection_receivers: Vec<_> = p2p_status_receivers
                .iter_mut()
                .filter_map(|(p, r)| {
                    r.mark_unchanged();
                    r.borrow().is_none().then_some((*p, r.clone()))
                })
                .collect();

            if pending_connection_receivers.is_empty() {
                break;
            }

            let disconnected_peers = pending_connection_receivers
                .iter()
                .map(|entry| entry.0)
                .collect::<Vec<PeerId>>();

            info!(
                target: LOG_NET_PEER_DKG,
                pending = ?disconnected_peers,
                "Waiting for all p2p connections to open..."
            );

            select! {
                _ = select_all(pending_connection_receivers.iter_mut().map(|r| Box::pin(r.1.changed()))) => {}
                () = sleep(Duration::from_secs(10)) => {}
            }
        }

        let checksum = params.peers.consensus_hash_sha256();

        info!(
            target: LOG_NET_PEER_DKG,
            "Comparing connection codes checksum {checksum}..."
        );

        connections.send(Recipient::Everyone, P2PMessage::Checksum(checksum));

        for peer in params
            .peer_ids()
            .into_iter()
            .filter(|p| *p != params.identity)
        {
            let peer_message = connections
                .receive_from_peer(peer)
                .await
                .context("Unexpected shutdown of p2p connections")?;

            if peer_message != P2PMessage::Checksum(checksum) {
                error!(
                    target: LOG_NET_PEER_DKG,
                    expected = ?P2PMessage::Checksum(checksum),
                    received = ?peer_message,
                    "Peer {peer} has sent invalid connection code checksum message"
                );

                bail!("Peer {peer} has sent invalid connection code checksum message");
            }

            info!(
                target: LOG_NET_PEER_DKG,
                "Peer {peer} has sent valid connection code checksum message"
            );
        }

        info!(
            target: LOG_NET_PEER_DKG,
            "Running config generation..."
        );

        let handle = DkgHandle::new(
            params.peer_ids().to_num_peers(),
            params.identity,
            &connections,
        );

        let (broadcast_sk, broadcast_pk) = secp256k1::generate_keypair(&mut OsRng);

        let broadcast_public_keys = handle.exchange_encodable(broadcast_pk).await?;

        info!(
            target: LOG_NET_PEER_DKG,
            "Running config generation for module of kind mint..."
        );

        let mint = crate::consensus::mint::distributed_gen(&handle).await?;

        info!(
            target: LOG_NET_PEER_DKG,
            "Running config generation for module of kind ln..."
        );

        let ln = crate::consensus::ln::distributed_gen(&handle).await?;

        info!(
            target: LOG_NET_PEER_DKG,
            "Running config generation for module of kind wallet..."
        );

        let wallet = crate::consensus::wallet::distributed_gen(&handle).await?;

        let cfg = ServerConfig::from(
            params.clone(),
            params.identity,
            broadcast_public_keys,
            broadcast_sk,
            mint,
            ln,
            wallet,
        );

        let checksum = cfg.consensus.consensus_hash_sha256();

        info!(
            target: LOG_NET_PEER_DKG,
            "Comparing consensus config checksum {checksum}..."
        );

        connections.send(Recipient::Everyone, P2PMessage::Checksum(checksum));

        for peer in params
            .peer_ids()
            .into_iter()
            .filter(|p| *p != params.identity)
        {
            let peer_message = connections
                .receive_from_peer(peer)
                .await
                .context("Unexpected shutdown of p2p connections")?;

            if peer_message != P2PMessage::Checksum(checksum) {
                warn!(
                    target: LOG_NET_PEER_DKG,
                    expected = ?P2PMessage::Checksum(checksum),
                    received = ?peer_message,
                    config = ?cfg.consensus,
                    "Peer {peer} has sent invalid consensus config checksum message"
                );

                bail!("Peer {peer} has sent invalid consensus config checksum message");
            }

            info!(
                target: LOG_NET_PEER_DKG,
                "Peer {peer} has sent valid consensus config checksum message"
            );
        }

        info!(
            target: LOG_NET_PEER_DKG,
            "Config generation has completed successfully!"
        );

        Ok(cfg)
    }
}

impl ConfigGenParams {
    pub fn peer_ids(&self) -> Vec<PeerId> {
        self.peers.keys().copied().collect()
    }

    pub fn iroh_pks(&self) -> BTreeMap<PeerId, iroh_base::PublicKey> {
        self.peers.iter().map(|(id, peer)| (*id, peer.pk)).collect()
    }
}
