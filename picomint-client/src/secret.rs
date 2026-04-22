//! Mnemonic-driven client-tree derivation on top of `picomint_core::secret::Secret`.
//!
//! ```text
//! mnemonic
//!   → Secret::new_root(seed)
//!   → .child(&federation_id)       // per-federation root
//!   → .child(&Path::{Mint,Wallet,Ln})
//! ```
//!
//! The per-federation root is never used as key material itself — all three
//! of its children are module secrets and exhaust the derivation tree.

pub use bip39::{Language, Mnemonic};
use picomint_core::config::FederationId;
pub use picomint_core::secret::Secret;
use picomint_encoding::{Decodable, Encodable};
use rand::{CryptoRng, RngCore};

const WORD_COUNT: usize = 12;

/// Per-module leaf under the per-federation root.
///
/// The encoded discriminant is hashed into the child secret, so variant order
/// is load-bearing — reordering silently re-keys every client.
#[derive(Copy, Clone, Debug, Encodable, Decodable)]
pub enum Path {
    Mint,
    Ln,
    Wallet,
}

/// Generate a fresh 12-word English BIP39 mnemonic.
pub fn random<R: RngCore + CryptoRng>(rng: &mut R) -> Mnemonic {
    Mnemonic::generate_in_with(rng, Language::English, WORD_COUNT)
        .expect("Failed to generate mnemonic, bad word count")
}

pub(crate) fn client_root(mnemonic: &Mnemonic, federation_id: FederationId) -> Secret {
    Secret::new_root(&mnemonic.to_seed_normalized("")).child(&federation_id)
}
