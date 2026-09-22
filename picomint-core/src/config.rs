use std::collections::BTreeMap;
use std::fmt::Debug;
use std::hash::Hash;

use bitcoin::Network;
use bitcoin::hashes::{Hash as BitcoinHash, sha256};
use derive_more::{Display, FromStr};
use iroh_base::PublicKey;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::NodeId;
use crate::ecash::config::{EcashConfigConsensus, EcashConfigPrivate};
use crate::invite::InviteCode;
use crate::lightning::config::LightningConfigConsensus;
use crate::onchain::config::{OnchainConfigConsensus, OnchainConfigPrivate};
use crate::version::ConsensusVersion;
use picomint_encoding::{Base32, Decodable, Encodable};

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

/// The mint's identity: the sha256 hash of its consensus config, so two
/// mints can only share an id by sharing every key and parameter. Hex.
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
    JsonSchema,
)]
pub struct MintId(#[schemars(with = "String")] pub sha256::Hash);

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
pub struct NodeConfigConsensus {
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

/// A node's whole config: the mint's consensus config plus this node's
/// keys. Persisted in the node database, printed by `backup` and taken
/// back unchanged by `setup restore`.
#[derive(Debug, Clone, Serialize, Deserialize, Encodable, Decodable)]
pub struct NodeConfig {
    /// Mint-wide config, identical across nodes
    pub consensus: NodeConfigConsensus,
    /// This node's secrets
    pub private: NodeConfigPrivate,
}

/// This node's secrets: its identity and the keys DKG dealt it.
#[derive(Debug, Clone, Serialize, Deserialize, Encodable, Decodable)]
pub struct NodeConfigPrivate {
    /// Our node id
    pub identity: NodeId,
    /// Secret key for our single iroh endpoint (p2p + api)
    pub iroh_sk: iroh_base::SecretKey,
    /// Secret key for the atomic broadcast to sign messages
    pub broadcast_secret_key: secp256k1::SecretKey,
    /// Private key material for the ecash module
    pub ecash: EcashConfigPrivate,
    /// Private key material for the onchain module
    pub onchain: OnchainConfigPrivate,
}

impl NodeConfig {
    /// An invite code this node serves: its iroh key, the mint id and the
    /// invite's id.
    pub fn get_invite_code(&self, invite_id: [u8; 16]) -> InviteCode {
        InviteCode::new(
            self.private.iroh_sk.public(),
            self.consensus.calculate_mint_id(),
            invite_id,
        )
    }
}

impl NodeConfigConsensus {
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

/// A node's setup code: its name, its iroh public key and, if this node set
/// them, the mint's name and size. Its operator hands it to every other
/// node's operator during the setup ceremony, and `setup add` takes it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Encodable, Decodable, Base32)]
pub struct NodeSetupCode {
    pub name: String,
    pub pk: PublicKey,
    pub mint_name: Option<String>,
    pub mint_size: Option<u8>,
}
