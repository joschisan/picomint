//! Gateway-module derivation tree. Only constructible via
//! [`ClientSecret::gw_module_secret`]; the path enum is private.
//!
//! [`ClientSecret::gw_module_secret`]: crate::secret::ClientSecret::gw_module_secret

use picomint_core::secp256k1::Keypair;
use picomint_core::secret::Secret;
use picomint_encoding::Encodable;

#[derive(Encodable)]
enum Path {
    Contract,
}

#[derive(Copy, Clone, Debug)]
pub struct GwSecret(Secret);

impl GwSecret {
    pub(crate) fn new(module_root: Secret) -> Self {
        Self(module_root)
    }

    /// The gateway's federation-facing identity keypair. Used as `claim_pk`
    /// on outgoing contracts, as the refund key on incoming contracts, and
    /// to sign forfeit messages on cancelled sends.
    pub fn contract_keypair(&self) -> Keypair {
        self.0.child(&Path::Contract).to_secp_keypair()
    }
}
