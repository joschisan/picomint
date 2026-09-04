//! Mnemonic-driven client-tree derivation on top of [`Secret`].
//!
//! [`ClientSecret`] is the per-mint root. Its typed accessors descend
//! into the four per-module subtrees (each owned by its own `<module>/secret.rs`
//! file); [`Path`] labels the module hop and is kept private so that tree can
//! only be traversed via the typed entry points below.

pub use bip39::{Language, Mnemonic};
use picomint_core::config::MintId;
pub use picomint_core::secret::Secret;
use picomint_encoding::Encodable;
use rand::{CryptoRng, RngCore};

use crate::ecash::EcashSecret;
use crate::gateway::GatewaySecret;
use crate::lightning::LightningSecret;
use crate::onchain::OnchainSecret;

const WORD_COUNT: usize = 12;

/// Per-module hop under the per-mint client root. The encoded
/// discriminant is hashed into the child secret, so variant order is
/// load-bearing — reordering silently re-keys every client.
///
/// `Core` is reserved for a future client-core secret; it has no consumer
/// today. `Gateway` is for the gateway-flavor Lightning module, which runs its own
/// key space distinct from the regular `Lightning` client.
#[derive(Copy, Clone, Debug, Encodable)]
enum Path {
    #[allow(dead_code)]
    Core,
    Mint,
    Onchain,
    Lightning,
    Gateway,
}

/// Per-mint client root secret, derived from `mnemonic → mint`.
/// Exposes typed accessors for each module's sub-secret.
#[derive(Copy, Clone, Debug)]
pub struct ClientSecret(Secret);

impl ClientSecret {
    pub fn new(mnemonic: &Mnemonic, mint: MintId) -> Self {
        Self(Secret::new_root(&mnemonic.to_entropy()).child(&mint))
    }

    pub fn ecash_secret(&self) -> EcashSecret {
        EcashSecret::new(self.0.child(&Path::Mint))
    }

    pub fn onchain_secret(&self) -> OnchainSecret {
        OnchainSecret::new(self.0.child(&Path::Onchain))
    }

    pub fn lightning_secret(&self) -> LightningSecret {
        LightningSecret::new(self.0.child(&Path::Lightning))
    }

    pub fn gateway_secret(&self) -> GatewaySecret {
        GatewaySecret::new(self.0.child(&Path::Gateway))
    }
}

/// Generate a fresh 12-word English BIP39 mnemonic.
pub fn random<R: RngCore + CryptoRng>(rng: &mut R) -> Mnemonic {
    Mnemonic::generate_in_with(rng, Language::English, WORD_COUNT)
        .expect("Failed to generate mnemonic, bad word count")
}
