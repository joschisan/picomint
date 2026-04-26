//! Concrete schnorr-backed [`Keychain`] holding the federation's public-key
//! set, this peer's identity, and this peer's secret key.
//!
//! Produces and verifies BIP340 schnorr signatures. The signature type is a
//! fixed-size byte array (`[u8; SIGNATURE_LEN]`), and the partial-multisig
//! type is a sparse `NodeMap` of those bytes.

use std::collections::BTreeMap;

use picomint_core::bitcoin::hashes::Hash;
use picomint_core::secp256k1::hashes::sha256;
use picomint_core::secp256k1::{schnorr, Message, PublicKey, SecretKey, SECP256K1};
use picomint_core::{NumPeers, NumPeersExt, PeerId};
use picomint_encoding::Encodable;

use crate::node::{Index, NodeMap};

/// Length in bytes of a serialized schnorr signature.
pub const SIGNATURE_LEN: usize = 64;

/// A single peer's signature over some message — fixed-size schnorr bytes.
pub type Signature = [u8; SIGNATURE_LEN];

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
            .serialize()
    }

    pub fn verify(&self, msg: &[u8], signature: &Signature, peer_id: PeerId) -> bool {
        let Some(pk) = self.public_keys.get(&peer_id) else {
            return false;
        };
        let Ok(sig) = schnorr::Signature::from_slice(signature) else {
            return false;
        };
        SECP256K1
            .verify_schnorr(&sig, &Self::message(msg), &pk.x_only_public_key().0)
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
    /// Used by the engine's session-header signing path which works in
    /// terms of `schnorr::Signature` rather than `[u8; 64]`.
    pub fn sign_typed<T: Encodable + ?Sized>(&self, message: &T) -> schnorr::Signature {
        self.secret_key
            .keypair(SECP256K1)
            .sign_schnorr(Self::message(&message.consensus_encode_to_vec()))
    }

    pub fn verify_typed<T: Encodable + ?Sized>(
        &self,
        message: &T,
        signature: &schnorr::Signature,
        peer_id: PeerId,
    ) -> bool {
        let Some(pk) = self.public_keys.get(&peer_id) else {
            return false;
        };
        SECP256K1
            .verify_schnorr(
                signature,
                &Self::message(&message.consensus_encode_to_vec()),
                &pk.x_only_public_key().0,
            )
            .is_ok()
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
