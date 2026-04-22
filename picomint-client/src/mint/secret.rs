//! Mint-module derivation tree. Only constructible via
//! [`ClientSecret::mint_module_secret`]; the path enum is private.
//!
//! [`ClientSecret::mint_module_secret`]: crate::secret::ClientSecret::mint_module_secret

use picomint_core::mint::Denomination;
use picomint_core::secp256k1::Keypair;
use picomint_core::secret::Secret;
use picomint_encoding::Encodable;
use tbs::BlindingKey;

#[derive(Encodable)]
enum Path {
    TweakFilter,
    NoteNonce,
    NoteBlinding,
}

#[derive(Copy, Clone, Debug)]
pub struct MintSecret(Secret);

impl MintSecret {
    pub(crate) fn new(module_root: Secret) -> Self {
        Self(module_root)
    }

    pub fn tweak_filter(&self) -> [u8; 32] {
        self.0.child(&Path::TweakFilter).to_bytes()
    }

    pub fn note_nonce_keypair(&self, denomination: Denomination, tweak: [u8; 16]) -> Keypair {
        self.0
            .child(&Path::NoteNonce)
            .child(&denomination)
            .child(&tweak)
            .to_secp_keypair()
    }

    pub fn note_blinding_key(&self, denomination: Denomination, tweak: [u8; 16]) -> BlindingKey {
        BlindingKey(
            self.0
                .child(&Path::NoteBlinding)
                .child(&denomination)
                .child(&tweak)
                .to_bls_scalar(),
        )
    }
}
