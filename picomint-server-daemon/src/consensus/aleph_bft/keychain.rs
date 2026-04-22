use std::sync::Arc;

use aleph_bft::Keychain as KeychainTrait;
use bitcoin::hashes::Hash;
use picomint_core::{NumPeersExt, PeerId, secp256k1};
use picomint_encoding::Encodable;
use secp256k1::hashes::sha256;
use secp256k1::{Message, schnorr};

use crate::config::ServerConfig;

/// AlephBFT keychain backed by the server config. All fields are derived on
/// access — the config is the single source of truth for the peer set,
/// broadcast keys, and federation id.
#[derive(Clone, Debug)]
pub struct Keychain {
    cfg: Arc<ServerConfig>,
}

impl Keychain {
    pub fn new(cfg: &ServerConfig) -> Self {
        Keychain {
            cfg: Arc::new(cfg.clone()),
        }
    }

    // Tagging messages with the federation id binds every broadcast signature
    // to the full consensus config, so a signature produced under one
    // federation cannot be replayed against another.
    fn tagged_message<T: Encodable + ?Sized>(&self, message: &T) -> Message {
        let tag = self.cfg.consensus.calculate_federation_id();
        Message::from_digest(
            (tag, message)
                .consensus_hash::<sha256::Hash>()
                .to_byte_array(),
        )
    }

    pub fn sign_schnorr<T: Encodable + ?Sized>(&self, message: &T) -> schnorr::Signature {
        self.cfg
            .private
            .broadcast_secret_key
            .keypair(secp256k1::SECP256K1)
            .sign_schnorr(self.tagged_message(message))
    }

    pub fn verify_schnorr<T: Encodable + ?Sized>(
        &self,
        message: &T,
        signature: &schnorr::Signature,
        peer_id: PeerId,
    ) -> bool {
        match self.cfg.consensus.peers.get(&peer_id) {
            Some(endpoint) => secp256k1::SECP256K1
                .verify_schnorr(
                    signature,
                    &self.tagged_message(message),
                    &endpoint.broadcast_pk.x_only_public_key().0,
                )
                .is_ok(),
            None => false,
        }
    }
}

impl aleph_bft::Index for Keychain {
    fn index(&self) -> aleph_bft::NodeIndex {
        self.cfg.private.identity.to_usize().into()
    }
}

#[async_trait::async_trait]
impl aleph_bft::Keychain for Keychain {
    type Signature = [u8; 64];

    fn node_count(&self) -> aleph_bft::NodeCount {
        self.cfg.consensus.peers.len().into()
    }

    fn sign(&self, message: &[u8]) -> Self::Signature {
        self.sign_schnorr(message).serialize()
    }

    fn verify(
        &self,
        message: &[u8],
        signature: &Self::Signature,
        node_index: aleph_bft::NodeIndex,
    ) -> bool {
        match schnorr::Signature::from_slice(signature) {
            Ok(sig) => self.verify_schnorr(message, &sig, super::to_peer_id(node_index)),
            Err(_) => false,
        }
    }
}

impl aleph_bft::MultiKeychain for Keychain {
    type PartialMultisignature = aleph_bft::NodeMap<[u8; 64]>;

    fn bootstrap_multi(
        &self,
        signature: &Self::Signature,
        index: aleph_bft::NodeIndex,
    ) -> Self::PartialMultisignature {
        let mut partial = aleph_bft::NodeMap::with_size(self.cfg.consensus.peers.len().into());

        partial.insert(index, *signature);

        partial
    }

    fn is_complete(&self, msg: &[u8], partial: &Self::PartialMultisignature) -> bool {
        if partial.iter().count() < self.cfg.consensus.peers.to_num_peers().threshold() {
            return false;
        }

        partial.iter().all(|(i, sgn)| self.verify(msg, sgn, i))
    }
}
