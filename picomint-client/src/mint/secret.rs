//! Mint-module derivation tree. Only constructible via
//! [`ClientSecret::mint_secret`]; the path enum is private.
//!
//! Both leaves hang off a per-denomination issuance counter. Keeping the
//! denomination in the path means a recovering wallet knows each candidate's
//! denomination before it asks, so the federation never has to supply it and
//! recovery verifies blind shares through the same path as normal issuance.
//!
//! [`ClientSecret::mint_secret`]: crate::secret::ClientSecret::mint_secret

use picomint_core::mint::Denomination;
use picomint_core::secp256k1::Keypair;
use picomint_core::secret::Secret;
use picomint_encoding::Encodable;
use tbs::BlindingKey;

#[derive(Encodable)]
enum Path {
    NoteNonce,
    NoteBlinding,
}

#[derive(Copy, Clone, Debug)]
pub struct MintSecret(Secret);

impl MintSecret {
    pub(crate) fn new(module_root: Secret) -> Self {
        Self(module_root)
    }

    pub fn note_nonce_keypair(&self, denomination: Denomination, counter: u64) -> Keypair {
        self.0
            .child(&Path::NoteNonce)
            .child(&denomination)
            .child(&counter)
            .to_secp_keypair()
    }

    pub fn note_blinding_key(&self, denomination: Denomination, counter: u64) -> BlindingKey {
        BlindingKey(
            self.0
                .child(&Path::NoteBlinding)
                .child(&denomination)
                .child(&counter)
                .to_bls_scalar(),
        )
    }
}
