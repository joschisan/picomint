use picomint_core::core::Account;
use picomint_core::ecash::{Denomination, ECashOutput, nonce_message};
use picomint_core::secp256k1::{Keypair, XOnlyPublicKey};
use picomint_encoding::{Decodable, Encodable};
use tbs::{BlindedMessage, BlindedSignature, BlindingKey, blind_message, unblind_signature};

use super::SpendableNote;
use super::secret::ECashSecret;

/// One counter's key material, before a denomination is attached.
///
/// The two paths that produce notes disagree only on where the denomination
/// comes from: issuance picks it up front, a restore scan learns it from the
/// federation after probing. Both meet here, and neither can derive a nonce
/// that depends on the answer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Encodable, Decodable)]
pub struct NoteIssuance {
    /// Balance the finished note settles into. Carried rather than derived,
    /// so a transaction whose outputs are not all destined for the same
    /// account still files each one where it belongs.
    pub account: Account,
    pub counter: u64,
    pub keypair: Keypair,
    pub blinding_key: BlindingKey,
}

impl NoteIssuance {
    pub fn new(account: Account, counter: u64, ecash_secret: &ECashSecret) -> Self {
        Self {
            account,
            counter,
            keypair: ecash_secret.note_nonce_keypair(account, counter),
            blinding_key: ecash_secret.note_blinding_key(account, counter),
        }
    }

    pub fn nonce(&self) -> XOnlyPublicKey {
        self.keypair.x_only_public_key().0
    }

    /// The expensive half of a candidate: two G1 scalar multiplications,
    /// roughly twenty times the cost of [`NoteIssuance::nonce`]. A restore
    /// scan derives it only for counters the federation has already reported
    /// as unspent.
    pub fn blinded_message(&self) -> BlindedMessage {
        blind_message(nonce_message(self.nonce()), self.blinding_key)
    }

    pub fn request(self, denomination: Denomination) -> NoteIssuanceRequest {
        NoteIssuanceRequest {
            denomination,
            issuance: self,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Encodable, Decodable)]
pub struct NoteIssuanceRequest {
    pub denomination: Denomination,
    pub issuance: NoteIssuance,
}

impl NoteIssuanceRequest {
    pub fn new(
        account: Account,
        denomination: Denomination,
        counter: u64,
        ecash_secret: &ECashSecret,
    ) -> Self {
        NoteIssuance::new(account, counter, ecash_secret).request(denomination)
    }

    pub fn output(&self) -> ECashOutput {
        ECashOutput {
            denomination: self.denomination,
            nonce: self.blinded_message(),
        }
    }

    pub fn finalize(&self, signature: BlindedSignature) -> SpendableNote {
        SpendableNote {
            denomination: self.denomination,
            keypair: self.issuance.keypair,
            signature: unblind_signature(self.issuance.blinding_key, signature),
        }
    }

    pub fn account(&self) -> Account {
        self.issuance.account
    }

    pub fn nonce(&self) -> XOnlyPublicKey {
        self.issuance.nonce()
    }

    pub fn blinded_message(&self) -> BlindedMessage {
        self.issuance.blinded_message()
    }
}
