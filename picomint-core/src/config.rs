use std::collections::BTreeMap;
use std::fmt::Debug;
use std::hash::Hash;

use bitcoin::Network;
use bitcoin::hashes::{Hash as BitcoinHash, sha256};
use derive_more::{Display, FromStr};
use serde::{Deserialize, Serialize};

use crate::NodeId;
use crate::ecash::config::EcashConfigConsensus;
use crate::lightning::config::LightningConfigConsensus;
use crate::onchain::config::OnchainConfigConsensus;
use crate::version::ConsensusVersion;
use picomint_encoding::{Decodable, Encodable};

// TODO: make configurable
/// How large a BFT unit's payload is meant to get. A unit stops taking items
/// once it reaches this, so it ends up at least this large and overshoots by
/// at most the item that got it there.
pub const BFT_UNIT_BYTE_TARGET: usize = 50_000;

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize, Encodable, Decodable)]
pub struct NodeEndpoint {
    /// The node's iroh API public key (QUIC transport identity).
    pub iroh_pk: iroh_base::PublicKey,
    /// The node's x-only secp256k1 public key used to authenticate
    /// atomic-broadcast messages.
    pub broadcast_pk: secp256k1::XOnlyPublicKey,
    /// The node's name.
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
pub struct MintId(pub sha256::Hash);

impl MintId {
    /// Random dummy id for testing
    pub fn dummy() -> Self {
        Self(sha256::Hash::from_byte_array([42; 32]))
    }
}

/// Mint-wide config.
///
/// Produced by DKG on the server side, served to clients via the core
/// [`CoreMethod::Config`] wire method, and stored in both the server and
/// client databases. Byte-for-byte identical on every node.
///
/// [`CoreMethod::Config`]: crate::methods::CoreMethod::Config
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize, Encodable, Decodable)]
pub struct ConsensusConfig {
    /// Per-node endpoint info (iroh pk, broadcast pk, name).
    pub nodes: BTreeMap<NodeId, NodeEndpoint>,
    /// Bitcoin network this mint operates on.
    pub network: Network,
    /// Mint name, chosen by the leader during setup.
    pub name: String,
    /// Consensus version this mint was created at, and so the version
    /// a node that has never voted counts as supporting. Set to the
    /// creating binary's [`CONSENSUS_VERSION`], which is what lets a fresh
    /// mint run the newest rules without a single vote being cast.
    ///
    /// [`CONSENSUS_VERSION`]: crate::version::CONSENSUS_VERSION
    pub default_version: ConsensusVersion,
    /// Ecash module config
    pub ecash: EcashConfigConsensus,
    /// Onchain module config
    pub onchain: OnchainConfigConsensus,
    /// Lightning module config
    pub lightning: LightningConfigConsensus,
}

impl ConsensusConfig {
    pub fn calculate_mint_id(&self) -> MintId {
        MintId(self.consensus_hash())
    }

    /// The nodes' iroh public keys — the node ids a client dials.
    pub fn iroh_pks(&self) -> BTreeMap<NodeId, iroh_base::PublicKey> {
        self.nodes
            .iter()
            .map(|entry| (*entry.0, entry.1.iroh_pk))
            .collect()
    }
}
