//! Mnemonic-driven client-tree derivation on top of [`Secret`].
//!
//! [`ClientSecret`] is the per-federation root. Its typed accessors descend
//! into the four per-module subtrees (each owned by its own `<module>/secret.rs`
//! file); [`Path`] labels the module hop and is kept private so that tree can
//! only be traversed via the typed entry points below.

pub use bip39::{Language, Mnemonic};
use picomint_core::config::FederationId;
pub use picomint_core::secret::Secret;
use picomint_encoding::Encodable;
use rand::{CryptoRng, RngCore};

use crate::gw::GwSecret;
use crate::ln::LnSecret;
use crate::mint::MintSecret;
use crate::wallet::WalletSecret;

const WORD_COUNT: usize = 12;

/// Per-module hop under the per-federation client root. The encoded
/// discriminant is hashed into the child secret, so variant order is
/// load-bearing — reordering silently re-keys every client.
///
/// `Core` is reserved for a future client-core secret; it has no consumer
/// today. `Gw` is for the gateway-flavor Lightning module, which runs its own
/// key space distinct from the regular `Ln` client.
#[derive(Copy, Clone, Debug, Encodable)]
enum Path {
    #[allow(dead_code)]
    Core,
    Mint,
    Wallet,
    Ln,
    Gw,
}

/// Per-federation client root secret, derived from `mnemonic → federation_id`.
/// Exposes typed accessors for each module's sub-secret.
#[derive(Copy, Clone, Debug)]
pub struct ClientSecret(Secret);

impl ClientSecret {
    pub fn new(mnemonic: &Mnemonic, federation_id: FederationId) -> Self {
        Self(Secret::new_root(&mnemonic.to_seed_normalized("")).child(&federation_id))
    }

    pub fn mint_secret(&self) -> MintSecret {
        MintSecret::new(self.0.child(&Path::Mint))
    }

    pub fn ln_secret(&self) -> LnSecret {
        LnSecret::new(self.0.child(&Path::Ln))
    }

    pub fn wallet_secret(&self) -> WalletSecret {
        WalletSecret::new(self.0.child(&Path::Wallet))
    }

    pub fn gw_secret(&self) -> GwSecret {
        GwSecret::new(self.0.child(&Path::Gw))
    }
}

/// Generate a fresh 12-word English BIP39 mnemonic.
pub fn random<R: RngCore + CryptoRng>(rng: &mut R) -> Mnemonic {
    Mnemonic::generate_in_with(rng, Language::English, WORD_COUNT)
        .expect("Failed to generate mnemonic, bad word count")
}
