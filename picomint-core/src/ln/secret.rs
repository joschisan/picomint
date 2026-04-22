//! Incoming-contract secret derivation.
//!
//! Both the contract creator (client receive path, recurringd) and the
//! claimant (client recover path) start from the same 32-byte ECDH output
//! and descend this tree to recover identical encryption seed, preimage, and
//! claim tweak. The path enum is private; callers use the typed accessors on
//! [`IncomingContractSecret`].

use crate::secp256k1::Scalar;
use crate::secret::Secret;
use picomint_encoding::Encodable;

#[derive(Encodable)]
enum Path {
    EncryptionSeed,
    Preimage,
    ClaimKey,
}

/// ECDH-rooted secret tree for an incoming Lightning contract.
#[derive(Copy, Clone, Debug)]
pub struct IncomingContractSecret(Secret);

impl IncomingContractSecret {
    pub fn new(shared_secret: [u8; 32]) -> Self {
        Self(Secret::new_root(&shared_secret))
    }

    pub fn encryption_seed(&self) -> [u8; 32] {
        self.0.child(&Path::EncryptionSeed).to_bytes()
    }

    pub fn preimage(&self) -> [u8; 32] {
        self.0.child(&Path::Preimage).to_bytes()
    }

    pub fn claim_tweak(&self) -> Scalar {
        self.0.child(&Path::ClaimKey).to_secp_scalar()
    }
}
