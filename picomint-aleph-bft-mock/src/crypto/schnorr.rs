//! Test fixtures for the concrete [`Schnorr`] keychain.
//!
//! The mock [`super::Keychain`] trait impl still exists (and is what the
//! aleph-bft tests use); these helpers are for consumer crates that have
//! already migrated off the trait abstraction onto the concrete struct.

use std::collections::BTreeMap;

use aleph_bft_types::{NumPeers, PeerId, Schnorr};
use picomint_core::secp256k1::{PublicKey, SECP256K1, SecretKey};

/// Deterministic [`SecretKey`] from a small integer seed — gives every test
/// a reproducible federation without touching an OS RNG.
fn deterministic_secret(seed: u8) -> SecretKey {
    let mut bytes = [1u8; 32];
    bytes[31] = seed.saturating_add(1);
    SecretKey::from_slice(&bytes).expect("non-zero 32-byte slice is a valid SecretKey")
}

/// Build a federation of `n` keychains with deterministic, distinct keypairs.
pub fn schnorr_set(n: NumPeers) -> Vec<Schnorr> {
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
        .map(|(i, sk)| Schnorr::new(public_keys.clone(), PeerId::from(i as u8), sk))
        .collect()
}

/// Build the keychain at index `peer_id` for a federation of size `n`.
pub fn schnorr(n: NumPeers, peer_id: PeerId) -> Schnorr {
    schnorr_set(n)
        .into_iter()
        .nth(peer_id.to_usize())
        .expect("peer_id within federation size")
}

/// Build a "bad" keychain whose secret key is *not* the one registered in
/// the federation's public-key set under `identity` — every signature it
/// produces fails verification under honest peers' keychains.
pub fn bad_schnorr(n: NumPeers, identity: PeerId) -> Schnorr {
    let public_keys = schnorr_set(n)[0].public_keys().clone();
    let bad_secret = deterministic_secret(0xff);
    Schnorr::new(public_keys, identity, bad_secret)
}
