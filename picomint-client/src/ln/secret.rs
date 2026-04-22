//! Lightning-module derivation tree. Only constructible via
//! [`ClientSecret::ln_secret`]; the path enum is private.
//!
//! [`ClientSecret::ln_secret`]: crate::secret::ClientSecret::ln_secret

use picomint_core::secp256k1::Keypair;
use picomint_core::secret::Secret;
use picomint_encoding::Encodable;

#[derive(Encodable)]
enum Path {
    Refund,
    Receive,
}

#[derive(Copy, Clone, Debug)]
pub struct LnSecret(Secret);

impl LnSecret {
    pub(crate) fn new(module_root: Secret) -> Self {
        Self(module_root)
    }

    pub fn refund_keypair(&self, tweak: &[u8; 16]) -> Keypair {
        self.0.child(&Path::Refund).child(tweak).to_secp_keypair()
    }

    pub fn receive_keypair(&self) -> Keypair {
        self.0.child(&Path::Receive).to_secp_keypair()
    }
}
