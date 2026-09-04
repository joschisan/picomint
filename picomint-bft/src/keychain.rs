use std::collections::BTreeMap;

use bitcoin::hashes::Hash;
use bitcoin::secp256k1::Message;
use picomint_core::NodeId;
use picomint_core::secp256k1::{Keypair, SECP256K1, XOnlyPublicKey, schnorr};
use picomint_encoding::Encodable;

/// Schnorr signing identity plus the mint's public-key set, indexed by
/// `NodeId`. Every node in a session shares the same pubkey map; only the
/// `keypair` differs.
#[derive(Clone)]
pub struct Keychain {
    keypair: Keypair,
    pubkeys: BTreeMap<NodeId, XOnlyPublicKey>,
}

impl Keychain {
    /// Construct a keychain from this node's own keypair and the mint's
    /// known public keys.
    pub fn new(keypair: Keypair, pubkeys: BTreeMap<NodeId, XOnlyPublicKey>) -> Self {
        Self { keypair, pubkeys }
    }

    /// Sign the consensus-hash of `(session, value)` with our schnorr
    /// key. The `session` prefix binds every signature to the consensus
    /// session it was produced under: a stale signature from session N
    /// arriving at a node in session N+1 will hash under a different
    /// tuple at the verifier and fail to match, so it's discarded.
    pub fn sign<E: Encodable>(&self, session: u32, value: &E) -> schnorr::Signature {
        self.keypair.sign_schnorr(Message::from_digest(
            (session, value).consensus_hash_sha256().to_byte_array(),
        ))
    }

    /// Verify `signature` is `node`'s schnorr signature over the
    /// consensus-hash of `(session, value)`. False for a node outside
    /// the keychain — `node` may come straight off the wire, and an
    /// unknown signer is just an invalid signature, never a panic.
    pub fn verify<E: Encodable>(
        &self,
        session: u32,
        value: &E,
        signature: &schnorr::Signature,
        node: NodeId,
    ) -> bool {
        let message =
            Message::from_digest((session, value).consensus_hash_sha256().to_byte_array());

        let Some(pk) = self.pubkeys.get(&node) else {
            return false;
        };

        SECP256K1.verify_schnorr(signature, &message, pk).is_ok()
    }
}
