//! Mint-module derivation tree. Only constructible via
//! [`ClientSecret::mint_secret`]; the path enum is private.
//!
//! Both leaves hang off a single wallet-wide issuance counter, so a counter
//! yields exactly one note and nothing here depends on the denomination. A
//! restoring wallet therefore cannot know a candidate's denomination on its
//! own and learns it from the federation, which reports it alongside
//! membership in `issuance_state`.
//!
//! [`ClientSecret::mint_secret`]: crate::secret::ClientSecret::mint_secret

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

    pub fn note_nonce_keypair(&self, counter: u64) -> Keypair {
        self.0
            .child(&Path::NoteNonce)
            .child(&counter)
            .to_secp_keypair()
    }

    pub fn note_blinding_key(&self, counter: u64) -> BlindingKey {
        BlindingKey(
            self.0
                .child(&Path::NoteBlinding)
                .child(&counter)
                .to_bls_scalar(),
        )
    }
}
