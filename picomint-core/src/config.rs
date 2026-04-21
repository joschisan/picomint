use std::collections::BTreeMap;
use std::fmt::Debug;
use std::hash::Hash;

use bitcoin::hashes::{Hash as BitcoinHash, sha256};
use derive_more::{Display, FromStr};
use serde::{Deserialize, Serialize};

use crate::PeerId;
use crate::ln::config::LightningConfigConsensus;
use crate::mint::config::MintConfigConsensus;
use crate::wallet::config::WalletConfigConsensus;
use picomint_encoding::{Decodable, Encodable};
use secp256k1::PublicKey;

// TODO: make configurable
/// This limits the RAM consumption of a AlephBFT Unit to roughly 50kB
pub const ALEPH_BFT_UNIT_BYTE_LIMIT: usize = 50_000;

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize, Encodable, Decodable)]
pub struct PeerEndpoint {
    /// The peer's iroh API public key
    pub node_id: iroh_base::PublicKey,
    /// The peer's name
    pub name: String,
}

#[derive(
    Debug,
    Copy,
    Serialize,
    Deserialize,
    Clone,
    Eq,
    Hash,
    PartialEq,
    Ord,
    PartialOrd,
    Encodable,
    Decodable,
    Display,
    FromStr,
)]
pub struct FederationId(pub sha256::Hash);

picomint_redb::consensus_key!(FederationId);

impl FederationId {
    /// Random dummy id for testing
    pub fn dummy() -> Self {
        Self(sha256::Hash::from_byte_array([42; 32]))
    }
}

/// Key under which the federation name can be sent to client in the `meta` part
/// of the config
pub const META_FEDERATION_NAME_KEY: &str = "federation_name";

/// Federation-wide config.
///
/// Produced by DKG on the server side, served to clients via the
/// [`METHOD_CLIENT_CONFIG`], and stored in both the server and client
/// databases. Byte-for-byte identical on every peer.
///
/// [`METHOD_CLIENT_CONFIG`]: crate::methods::METHOD_CLIENT_CONFIG
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize, Encodable, Decodable)]
pub struct ConsensusConfig {
    /// Public keys for the atomic broadcast to authenticate messages
    pub broadcast_public_keys: BTreeMap<PeerId, PublicKey>,
    /// Number of rounds per session
    pub broadcast_rounds_per_session: u16,
    /// Public keys + names for every peer's single iroh endpoint (p2p + api).
    pub iroh_endpoints: BTreeMap<PeerId, PeerEndpoint>,
    /// Free-form federation metadata (federation name, etc.)
    pub meta: BTreeMap<String, String>,
    /// Mint module config
    pub mint: MintConfigConsensus,
    /// Lightning module config
    pub ln: LightningConfigConsensus,
    /// Wallet module config
    pub wallet: WalletConfigConsensus,
}

picomint_redb::consensus_value!(ConsensusConfig);

impl ConsensusConfig {
    pub fn calculate_federation_id(&self) -> FederationId {
        FederationId(self.iroh_endpoints.consensus_hash())
    }

    pub fn federation_name(&self) -> Option<String> {
        self.meta.get(META_FEDERATION_NAME_KEY).cloned()
    }
}
