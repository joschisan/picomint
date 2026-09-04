//! Lightning-module derivation tree. Only constructible via
//! [`ClientSecret::lightning_secret`]; the path enum is private.
//!
//! [`ClientSecret::lightning_secret`]: crate::secret::ClientSecret::lightning_secret

use picomint_core::core::Account;
use picomint_core::secp256k1::Keypair;
use picomint_core::secret::Secret;
use picomint_encoding::Encodable;

#[derive(Encodable)]
enum Path {
    Receive,
}

#[derive(Copy, Clone, Debug)]
pub struct LightningSecret(Secret);

impl LightningSecret {
    pub(crate) fn new(module_root: Secret) -> Self {
        Self(module_root)
    }

    /// The account's static receive key. Contracts are locked to a tweak of
    /// this, so it is what an lnurl names and what the stream scanner trials
    /// each streamed contract against.
    pub fn receive_keypair(&self, account: Account) -> Keypair {
        self.0
            .child(&account)
            .child(&Path::Receive)
            .to_secp_keypair()
    }
}
