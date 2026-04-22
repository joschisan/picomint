//! Mnemonic-driven client-tree derivation on top of [`Secret`].
//!
//! ```text
//! mnemonic
//!   → Secret::new_root(seed)
//!   → .child(&federation_id)       // per-federation root
//!   → .{mint,ln,wallet,gw}_module_secret()
//! ```
//!
//! The per-module accessors + the path enum they dispatch to live on
//! [`Secret`] in `picomint-core` (next to the primitive); this module just
//! adds the mnemonic + per-federation root layer.

pub use bip39::{Language, Mnemonic};
use picomint_core::config::FederationId;
pub use picomint_core::secret::Secret;
use rand::{CryptoRng, RngCore};

const WORD_COUNT: usize = 12;

/// Generate a fresh 12-word English BIP39 mnemonic.
pub fn random<R: RngCore + CryptoRng>(rng: &mut R) -> Mnemonic {
    Mnemonic::generate_in_with(rng, Language::English, WORD_COUNT)
        .expect("Failed to generate mnemonic, bad word count")
}

pub(crate) fn client_root(mnemonic: &Mnemonic, federation_id: FederationId) -> Secret {
    Secret::new_root(&mnemonic.to_seed_normalized("")).child(&federation_id)
}
