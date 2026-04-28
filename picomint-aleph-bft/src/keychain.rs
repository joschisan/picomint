use std::collections::BTreeMap;

use bitcoin::hashes::Hash;
use bitcoin::secp256k1::Message;
use picomint_core::PeerId;
use picomint_core::secp256k1::{Keypair, SECP256K1, XOnlyPublicKey, schnorr};

use crate::unit::UnitHash;

/// Schnorr signing identity plus the federation's public-key set, indexed by
/// `PeerId`. Every peer in a session shares the same pubkey map; only the
/// `keypair` differs.
#[derive(Clone)]
pub struct Keychain {
    keypair: Keypair,
    pubkeys: BTreeMap<PeerId, XOnlyPublicKey>,
}

impl Keychain {
    /// Construct a keychain from this peer's own keypair and the federation's
    /// known public keys.
    pub fn new(keypair: Keypair, pubkeys: BTreeMap<PeerId, XOnlyPublicKey>) -> Self {
        Self { keypair, pubkeys }
    }

    /// Sign a unit hash with our schnorr key.
    pub fn sign(&self, unit_hash: &UnitHash) -> schnorr::Signature {
        self.keypair
            .sign_schnorr(Message::from_digest(unit_hash.to_byte_array()))
    }

    /// Verify `signature` is `peer`'s schnorr signature over `unit_hash`.
    pub fn verify(
        &self,
        unit_hash: &UnitHash,
        signature: &schnorr::Signature,
        peer: PeerId,
    ) -> bool {
        let pubkey = self
            .pubkeys
            .get(&peer)
            .expect("verify is only ever called with peers from our keychain");

        SECP256K1
            .verify_schnorr(
                signature,
                &Message::from_digest(unit_hash.to_byte_array()),
                pubkey,
            )
            .is_ok()
    }
}
