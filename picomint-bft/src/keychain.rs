use std::collections::BTreeMap;

use bitcoin::hashes::Hash;
use bitcoin::secp256k1::Message;
use picomint_core::PeerId;
use picomint_core::secp256k1::{Keypair, SECP256K1, XOnlyPublicKey, schnorr};
use picomint_encoding::Encodable;

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

    /// Sign the consensus-hash of `value` with our schnorr key.
    pub fn sign<E: Encodable>(&self, value: &E) -> schnorr::Signature {
        self.keypair.sign_schnorr(Message::from_digest(
            value.consensus_hash_sha256().to_byte_array(),
        ))
    }

    /// Verify `signature` is `peer`'s schnorr signature over the
    /// consensus-hash of `value`.
    pub fn verify<E: Encodable>(
        &self,
        value: &E,
        signature: &schnorr::Signature,
        peer: PeerId,
    ) -> bool {
        let message = Message::from_digest(value.consensus_hash_sha256().to_byte_array());

        let pk = self
            .pubkeys
            .get(&peer)
            .expect("verify is only ever called with peers from our keychain");

        SECP256K1.verify_schnorr(signature, &message, pk).is_ok()
    }
}
