//! Wallet-module derivation tree. Only constructible via
//! [`ClientSecret::onchain_secret`]; the path enum is private.
//!
//! [`ClientSecret::onchain_secret`]: crate::secret::ClientSecret::onchain_secret

use picomint_core::core::Account;
use picomint_core::secp256k1::Keypair;
use picomint_core::secret::Secret;
use picomint_encoding::Encodable;

#[derive(Encodable)]
enum Path {
    Address,
}

#[derive(Copy, Clone, Debug)]
pub struct OnchainSecret(Secret);

impl OnchainSecret {
    pub(crate) fn new(module_root: Secret) -> Self {
        Self(module_root)
    }

    pub fn address_keypair(&self, account: Account, index: u64) -> Keypair {
        self.0
            .child(&account)
            .child(&Path::Address)
            .child(&index)
            .to_secp_keypair()
    }
}
