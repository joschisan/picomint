use picomint_core::mint::{Denomination, MintOutput, nonce_message};
use picomint_core::secp256k1::{Keypair, XOnlyPublicKey};
use picomint_encoding::{Decodable, Encodable};
use tbs::{BlindedMessage, BlindedSignature, BlindingKey, blind_message, unblind_signature};

use super::SpendableNote;
use super::secret::MintSecret;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Encodable, Decodable)]
pub struct NoteIssuanceRequest {
    pub denomination: Denomination,
    pub counter: u64,
    pub keypair: Keypair,
    pub blinding_key: BlindingKey,
}

impl NoteIssuanceRequest {
    pub fn new(denomination: Denomination, counter: u64, mint_secret: &MintSecret) -> Self {
        Self {
            denomination,
            counter,
            keypair: mint_secret.note_nonce_keypair(denomination, counter),
            blinding_key: mint_secret.note_blinding_key(denomination, counter),
        }
    }

    pub fn output(&self) -> MintOutput {
        MintOutput {
            denomination: self.denomination,
            nonce: self.blinded_message(),
        }
    }

    pub fn finalize(&self, signature: BlindedSignature) -> SpendableNote {
        SpendableNote {
            denomination: self.denomination,
            keypair: self.keypair,
            signature: unblind_signature(self.blinding_key, signature),
        }
    }

    pub fn nonce(&self) -> XOnlyPublicKey {
        self.keypair.x_only_public_key().0
    }

    /// The expensive half of a candidate: two G1 scalar multiplications,
    /// roughly twenty times the cost of [`NoteIssuanceRequest::nonce`]. A
    /// recovery scan derives it only for counters the federation has already
    /// reported as unspent.
    pub fn blinded_message(&self) -> BlindedMessage {
        blind_message(nonce_message(self.nonce()), self.blinding_key)
    }
}
