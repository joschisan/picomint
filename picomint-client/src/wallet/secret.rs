//! Wallet-module derivation tree. Only constructible via
//! [`ClientSecret::wallet_module_secret`]; the path enum is private.
//!
//! [`ClientSecret::wallet_module_secret`]: crate::secret::ClientSecret::wallet_module_secret

use picomint_core::secp256k1::Keypair;
use picomint_core::secret::Secret;
use picomint_encoding::Encodable;

#[derive(Encodable)]
enum Path {
    Address,
}

#[derive(Copy, Clone, Debug)]
pub struct WalletSecret(Secret);

impl WalletSecret {
    pub(crate) fn new(module_root: Secret) -> Self {
        Self(module_root)
    }

    pub fn address_keypair(&self, index: u64) -> Keypair {
        self.0.child(&Path::Address).child(&index).to_secp_keypair()
    }
}
