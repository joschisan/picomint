//! Swap-module derivation tree. Only constructible via
//! [`ClientSecret::swap_secret`]; the path enum is private.
//!
//! [`ClientSecret::swap_secret`]: crate::secret::ClientSecret::swap_secret

use picomint_core::core::Account;
use picomint_core::secp256k1::Keypair;
use picomint_core::secret::Secret;
use picomint_encoding::Encodable;

#[derive(Encodable)]
enum Path {
    Receive,
    Claim,
}

#[derive(Copy, Clone, Debug)]
pub struct SwapSecret(Secret);

impl SwapSecret {
    pub(crate) fn new(module_root: Secret) -> Self {
        Self(module_root)
    }

    /// The account's static receive key: what its swap address names, and
    /// what the stream scanner trials each streamed contract against.
    pub fn receive_keypair(&self, account: Account) -> Keypair {
        self.0
            .child(&account)
            .child(&Path::Receive)
            .to_secp_keypair()
    }

    /// A broker's mint-facing identity keypair: the key senders lock send
    /// contracts to, so it has to be static and public.
    pub fn claim_keypair(&self) -> Keypair {
        self.0.child(&Path::Claim).to_secp_keypair()
    }
}
