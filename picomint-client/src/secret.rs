//! Mnemonic-driven client-tree derivation on top of `picomint_core::secret::Secret`.
//!
//! ```text
//! mnemonic
//!   → Secret::new_root(seed)
//!   → .child(&federation_id)       // per-federation root
//!   → .child(&Path::{Core,Mint,Ln,Wallet,Gw})
//! ```
//!
//! [`Path`] is kept private so the derivation tree stays an internal detail of
//! this module; callers consume typed getters like [`mint_module_secret`] and
//! can't accidentally invent a new path or reorder variants in transit.

pub use bip39::{Language, Mnemonic};
use picomint_core::config::FederationId;
pub use picomint_core::secret::Secret;
use picomint_encoding::{Decodable, Encodable};
use rand::{CryptoRng, RngCore};

const WORD_COUNT: usize = 12;

/// Leaf label under the per-federation root. The encoded discriminant is
/// hashed into the child secret, so variant order is load-bearing —
/// reordering silently re-keys every client.
///
/// `Core` is reserved for a future client-core secret (e.g. a recurring-
/// payments identity key); it has no consumer today. `Gw` is for the
/// gateway-flavor Lightning module, which runs its own key space distinct
/// from the regular Ln client.
#[derive(Copy, Clone, Debug, Encodable, Decodable)]
enum Path {
    Core,
    Mint,
    Ln,
    Wallet,
    Gw,
}

/// Generate a fresh 12-word English BIP39 mnemonic.
pub fn random<R: RngCore + CryptoRng>(rng: &mut R) -> Mnemonic {
    Mnemonic::generate_in_with(rng, Language::English, WORD_COUNT)
        .expect("Failed to generate mnemonic, bad word count")
}

pub(crate) fn client_root(mnemonic: &Mnemonic, federation_id: FederationId) -> Secret {
    Secret::new_root(&mnemonic.to_seed_normalized("")).child(&federation_id)
}

pub fn mint_module_secret(root: &Secret) -> Secret {
    root.child(&Path::Mint)
}

pub fn ln_module_secret(root: &Secret) -> Secret {
    root.child(&Path::Ln)
}

pub fn wallet_module_secret(root: &Secret) -> Secret {
    root.child(&Path::Wallet)
}

pub fn gw_module_secret(root: &Secret) -> Secret {
    root.child(&Path::Gw)
}
