use std::collections::BTreeMap;
use std::fmt::Debug;
use std::hash::Hash;

use bitcoin::Network;
use bitcoin::hashes::{Hash as BitcoinHash, sha256};
use derive_more::{Display, FromStr};
use serde::{Deserialize, Serialize};

use crate::PeerId;
use crate::ln::config::LightningConfigConsensus;
use crate::mint::config::MintConfigConsensus;
use crate::version::ConsensusVersion;
use crate::wallet::config::WalletConfigConsensus;
use picomint_encoding::{Decodable, Encodable};

// TODO: make configurable
/// How large a BFT unit's payload is meant to get. A unit stops taking items
/// once it reaches this, so it ends up at least this large and overshoots by
/// at most the item that got it there.
pub const BFT_UNIT_BYTE_TARGET: usize = 50_000;

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize, Encodable, Decodable)]
pub struct PeerEndpoint {
    /// The peer's iroh API public key (QUIC transport identity).
    pub iroh_pk: iroh_base::PublicKey,
    /// The peer's x-only secp256k1 public key used to authenticate
    /// atomic-broadcast messages.
    pub broadcast_pk: secp256k1::XOnlyPublicKey,
    /// The peer's name.
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

impl FederationId {
    /// Random dummy id for testing
    pub fn dummy() -> Self {
        Self(sha256::Hash::from_byte_array([42; 32]))
    }
}

/// Federation-wide config.
///
/// Produced by DKG on the server side, served to clients via the core
/// [`CoreMethod::Config`] wire method, and stored in both the server and
/// client databases. Byte-for-byte identical on every peer.
///
/// [`CoreMethod::Config`]: crate::methods::CoreMethod::Config
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize, Encodable, Decodable)]
pub struct ConsensusConfig {
    /// Per-peer endpoint info (iroh pk, broadcast pk, name).
    pub peers: BTreeMap<PeerId, PeerEndpoint>,
    /// Bitcoin network this federation operates on.
    pub network: Network,
    /// Federation name, chosen by the lead guardian during setup.
    pub name: String,
    /// Consensus version this federation was created at, and so the version
    /// a guardian that has never voted counts as supporting. Set to the
    /// creating binary's [`CONSENSUS_VERSION`], which is what lets a fresh
    /// federation run the newest rules without a single vote being cast.
    ///
    /// [`CONSENSUS_VERSION`]: crate::version::CONSENSUS_VERSION
    pub default_version: ConsensusVersion,
    /// Mint module config
    pub mint: MintConfigConsensus,
    /// Wallet module config
    pub wallet: WalletConfigConsensus,
    /// Lightning module config
    pub ln: LightningConfigConsensus,
}

impl ConsensusConfig {
    pub fn calculate_federation_id(&self) -> FederationId {
        FederationId(self.consensus_hash())
    }
}
