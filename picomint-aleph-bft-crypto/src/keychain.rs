//! Concrete schnorr-backed [`Keychain`] holding the federation's public-key
//! set, this peer's identity, and this peer's secret key.
//!
//! Produces and verifies BIP340 schnorr signatures. The signature type is
//! [`secp256k1::schnorr::Signature`] (re-exported as [`Signature`]); the
//! partial-multisig type is a sparse [`NodeMap`] of those signatures.

use std::collections::BTreeMap;

pub use picomint_core::secp256k1::schnorr::Signature;

use picomint_core::bitcoin::hashes::Hash;
use picomint_core::secp256k1::hashes::sha256;
use picomint_core::secp256k1::{Message, PublicKey, SecretKey, SECP256K1};
use picomint_core::{NumPeers, NumPeersExt, PeerId};
use picomint_encoding::Encodable;

use crate::node::{Index, NodeMap};

/// A sparse map of peer signatures forming a partial multisignature.
pub type PartialMultisignature = NodeMap<Signature>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Keychain {
    public_keys: BTreeMap<PeerId, PublicKey>,
    identity: PeerId,
    secret_key: SecretKey,
}

impl Keychain {
    pub fn new(
        public_keys: BTreeMap<PeerId, PublicKey>,
        identity: PeerId,
        secret_key: SecretKey,
    ) -> Self {
        Self {
            public_keys,
            identity,
            secret_key,
        }
    }

    pub fn identity(&self) -> PeerId {
        self.identity
    }

    pub fn public_keys(&self) -> &BTreeMap<PeerId, PublicKey> {
        &self.public_keys
    }

    pub fn node_count(&self) -> NumPeers {
        self.public_keys.to_num_peers()
    }

    fn message(bytes: &[u8]) -> Message {
        Message::from_digest(<sha256::Hash as Hash>::hash(bytes).to_byte_array())
    }

    pub fn sign(&self, msg: &[u8]) -> Signature {
        self.secret_key
            .keypair(SECP256K1)
            .sign_schnorr(Self::message(msg))
    }

    pub fn verify(&self, msg: &[u8], signature: &Signature, peer_id: PeerId) -> bool {
        let Some(pk) = self.public_keys.get(&peer_id) else {
            return false;
        };
        SECP256K1
            .verify_schnorr(signature, &Self::message(msg), &pk.x_only_public_key().0)
            .is_ok()
    }

    pub fn bootstrap_multi(&self, signature: &Signature, index: PeerId) -> PartialMultisignature {
        let mut partial = PartialMultisignature::with_size(self.node_count());
        partial.insert(index, *signature);
        partial
    }

    pub fn is_complete(&self, msg: &[u8], partial: &PartialMultisignature) -> bool {
        if partial.iter().count() < self.node_count().threshold() {
            return false;
        }
        partial.iter().all(|(i, sig)| self.verify(msg, sig, i))
    }

    /// Sign a typed message — encodes via [`Encodable`] then schnorr-signs.
    pub fn sign_typed<T: Encodable + ?Sized>(&self, message: &T) -> Signature {
        self.sign(&message.consensus_encode_to_vec())
    }

    pub fn verify_typed<T: Encodable + ?Sized>(
        &self,
        message: &T,
        signature: &Signature,
        peer_id: PeerId,
    ) -> bool {
        self.verify(&message.consensus_encode_to_vec(), signature, peer_id)
    }
}

impl Index for Keychain {
    fn index(&self) -> PeerId {
        self.identity
    }
}

pub fn add_partial_signature(
    mut partial: PartialMultisignature,
    signature: &Signature,
    index: PeerId,
) -> PartialMultisignature {
    partial.insert(index, *signature);
    partial
}
