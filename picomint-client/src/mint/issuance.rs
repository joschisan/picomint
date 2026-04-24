use bitcoin_hashes::{Hash, hash160, sha256};
use picomint_core::mint::{Denomination, MintOutput, nonce_message};
use picomint_core::secp256k1::{Keypair, PublicKey};
use picomint_encoding::{Decodable, Encodable};
use tbs::{BlindedMessage, BlindedSignature, BlindingKey, blind_message, unblind_signature};

use super::SpendableNote;
use super::secret::MintSecret;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Encodable, Decodable)]
pub struct NoteIssuanceRequest {
    pub denomination: Denomination,
    pub tweak: [u8; 16],
    pub keypair: Keypair,
    pub blinding_key: BlindingKey,
}

impl NoteIssuanceRequest {
    pub fn new(denomination: Denomination, tweak: [u8; 16], mint_secret: &MintSecret) -> Self {
        let secret = output_secret(denomination, tweak, mint_secret);

        Self {
            denomination,
            tweak,
            keypair: keypair(&secret),
            blinding_key: blinding_key(&secret),
        }
    }

    pub fn output(&self) -> MintOutput {
        MintOutput {
            denomination: self.denomination,
            nonce: self.blinded_message(),
            tweak: self.tweak,
        }
    }

    pub fn finalize(&self, signature: BlindedSignature) -> SpendableNote {
        SpendableNote {
            denomination: self.denomination,
            keypair: self.keypair,
            signature: unblind_signature(self.blinding_key, signature),
        }
    }

    pub fn blinded_message(&self) -> BlindedMessage {
        blind_message(nonce_message(self.keypair.public_key()), self.blinding_key)
    }
}

pub fn check_tweak(tweak: [u8; 16], seed: [u8; 32]) -> bool {
    (seed, tweak)
        .consensus_hash::<sha256::Hash>()
        .to_byte_array()
        .iter()
        .take(2)
        .all(|b| *b == 0)
}

// ============ Validation Functions ============

pub fn check_nonce(secret: &OutputSecret, nonce_hash: hash160::Hash) -> bool {
    blinded_message(secret).consensus_hash::<hash160::Hash>() == nonce_hash
}

// ============ Core Crypto Functions ============

pub struct OutputSecret {
    denomination: Denomination,
    tweak: [u8; 16],
    mint_secret: MintSecret,
}

pub fn output_secret(
    denomination: Denomination,
    tweak: [u8; 16],
    mint_secret: &MintSecret,
) -> OutputSecret {
    OutputSecret {
        denomination,
        tweak,
        mint_secret: *mint_secret,
    }
}

fn keypair(secret: &OutputSecret) -> Keypair {
    secret
        .mint_secret
        .note_nonce_keypair(secret.denomination, secret.tweak)
}

pub fn nonce(secret: &OutputSecret) -> PublicKey {
    keypair(secret).public_key()
}

fn blinding_key(secret: &OutputSecret) -> BlindingKey {
    secret
        .mint_secret
        .note_blinding_key(secret.denomination, secret.tweak)
}

pub fn blinded_message(secret: &OutputSecret) -> BlindedMessage {
    blind_message(nonce_message(nonce(secret)), blinding_key(secret))
}
