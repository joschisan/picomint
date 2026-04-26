//! Test fixtures for the concrete [`Keychain`].
//!
//! Used by aleph-bft / aleph-bft-rmc tests to build a federation of
//! deterministic-keyed keychains, plus a "bad" keychain whose secret key
//! does not match the one registered under its identity.

use std::collections::BTreeMap;

use aleph_bft_types::{Keychain, NumPeers, PeerId};
use picomint_core::secp256k1::{PublicKey, SecretKey, SECP256K1};

/// Deterministic [`SecretKey`] from a small integer seed — gives every test
/// a reproducible federation without touching an OS RNG.
fn deterministic_secret(seed: u8) -> SecretKey {
    let mut bytes = [1u8; 32];
    bytes[31] = seed.saturating_add(1);
    SecretKey::from_slice(&bytes).expect("non-zero 32-byte slice is a valid SecretKey")
}

/// Build a federation of `n` keychains with deterministic, distinct keypairs.
pub fn keychain_set(n: NumPeers) -> Vec<Keychain> {
    let secrets: Vec<SecretKey> = (0..n.total())
        .map(|i| deterministic_secret(i as u8))
        .collect();
    let public_keys: BTreeMap<PeerId, PublicKey> = secrets
        .iter()
        .enumerate()
        .map(|(i, sk)| (PeerId::from(i as u8), sk.public_key(SECP256K1)))
        .collect();
    secrets
        .into_iter()
        .enumerate()
        .map(|(i, sk)| Keychain::new(public_keys.clone(), PeerId::from(i as u8), sk))
        .collect()
}

/// Build the keychain at index `peer_id` for a federation of size `n`.
pub fn keychain(n: NumPeers, peer_id: PeerId) -> Keychain {
    keychain_set(n)
        .into_iter()
        .nth(peer_id.to_usize())
        .expect("peer_id within federation size")
}

/// Build a "bad" keychain whose secret key is *not* the one registered in
/// the federation's public-key set under `identity` — every signature it
/// produces fails verification under honest peers' keychains.
pub fn bad_keychain(n: NumPeers, identity: PeerId) -> Keychain {
    let public_keys = keychain_set(n)[0].public_keys().clone();
    let bad_secret = deterministic_secret(0xff);
    Keychain::new(public_keys, identity, bad_secret)
}
