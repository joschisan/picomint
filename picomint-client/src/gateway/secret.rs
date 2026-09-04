//! Gateway-module derivation tree. Only constructible via
//! [`ClientSecret::gateway_secret`]; the path enum is private.
//!
//! [`ClientSecret::gateway_secret`]: crate::secret::ClientSecret::gateway_secret

use picomint_core::secp256k1::Keypair;
use picomint_core::secret::Secret;
use picomint_encoding::Encodable;

#[derive(Encodable)]
enum Path {
    Contract,
}

#[derive(Copy, Clone, Debug)]
pub struct GatewaySecret(Secret);

impl GatewaySecret {
    pub(crate) fn new(module_root: Secret) -> Self {
        Self(module_root)
    }

    /// The gateway's federation-facing identity keypair. Used as `claim_pk`
    /// on outgoing contracts and to sign forfeit messages on cancelled
    /// sends — both places a sender has to name the gateway in advance, so
    /// both have to be static and public.
    ///
    /// Incoming contracts do not use it: their refund key is fresh per
    /// contract, since the gateway picks it at funding time and nobody else
    /// needs to predict it.
    pub fn contract_keypair(&self) -> Keypair {
        self.0.child(&Path::Contract).to_secp_keypair()
    }
}
