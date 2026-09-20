//! Receive-contract secret derivation.
//!
//! The sender and the recipient start from the same 32-byte ECDH output
//! and descend this tree to the same claim tweak: the sender applies it to
//! the address's public key, the recipient to its secret key.

use crate::secp256k1::Scalar;
use crate::secret::Secret;
use picomint_encoding::Encodable;

#[derive(Encodable)]
enum Path {
    ClaimKey,
}

/// ECDH-rooted secret tree for a receive contract.
#[derive(Copy, Clone, Debug)]
pub struct ReceiveContractSecret(Secret);

impl ReceiveContractSecret {
    pub fn new(shared_secret: [u8; 32]) -> Self {
        Self(Secret::new_root(&shared_secret))
    }

    pub fn claim_tweak(&self) -> Scalar {
        self.0.child(&Path::ClaimKey).to_secp_scalar()
    }
}
