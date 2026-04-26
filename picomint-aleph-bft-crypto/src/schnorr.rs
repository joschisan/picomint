//! Concrete schnorr-backed [`Schnorr`] keychain that holds the federation's
//! public-key set, this peer's identity, and this peer's secret key.
//!
//! Currently lives next to the existing [`Keychain`] trait — it implements
//! both [`Keychain`] and [`MultiKeychain`] so callers can keep flowing
//! through the trait abstraction. Once all internal callers migrate to use
//! `Schnorr` directly, the traits will be deleted and this struct will be
//! renamed to plain `Keychain`.

use std::collections::BTreeMap;

use picomint_core::bitcoin::hashes::Hash;
use picomint_core::secp256k1::hashes::sha256;
use picomint_core::secp256k1::{Message, PublicKey, SECP256K1, SecretKey, schnorr};
use picomint_core::{NumPeers, NumPeersExt, PeerId};
use picomint_encoding::Encodable;

use crate::node::{Index, NodeMap};
use crate::signature::{Keychain, MultiKeychain, SignatureSet};

/// Length in bytes of a serialized schnorr signature.
pub const SIGNATURE_LEN: usize = 64;

#[derive(Clone, Debug)]
pub struct Schnorr {
    public_keys: BTreeMap<PeerId, PublicKey>,
    identity: PeerId,
    secret_key: SecretKey,
}

impl Schnorr {
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

    fn message(bytes: &[u8]) -> Message {
        Message::from_digest(<sha256::Hash as Hash>::hash(bytes).to_byte_array())
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

impl Index for Schnorr {
    fn index(&self) -> PeerId {
        self.identity
    }
}

impl Keychain for Schnorr {
    type Signature = [u8; SIGNATURE_LEN];

    fn node_count(&self) -> NumPeers {
        self.public_keys.to_num_peers()
    }

    fn sign(&self, msg: &[u8]) -> Self::Signature {
        self.secret_key
            .keypair(SECP256K1)
            .sign_schnorr(Self::message(msg))
            .serialize()
    }

    fn verify(&self, msg: &[u8], signature: &Self::Signature, peer_id: PeerId) -> bool {
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
}

impl MultiKeychain for Schnorr {
    type PartialMultisignature = NodeMap<[u8; SIGNATURE_LEN]>;

    fn bootstrap_multi(
        &self,
        signature: &Self::Signature,
        index: PeerId,
    ) -> Self::PartialMultisignature {
        let mut partial = SignatureSet::with_size(self.node_count());
        partial.insert(index, *signature);
        partial
    }

    fn is_complete(&self, msg: &[u8], partial: &Self::PartialMultisignature) -> bool {
        if partial.iter().count() < self.node_count().threshold() {
            return false;
        }
        partial.iter().all(|(i, sig)| self.verify(msg, sig, i))
    }
}
