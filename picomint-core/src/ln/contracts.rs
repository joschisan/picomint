use crate::Amount;
use bitcoin::hashes::sha256;
use bitcoin::secp256k1;
use picomint_encoding::{Decodable, Encodable};
use secp256k1::schnorr::Signature;
use secp256k1::{Message, PublicKey, XOnlyPublicKey};
use serde::{Deserialize, Serialize};
use tpe::{
    AggregateDecryptionKey, AggregatePublicKey, CipherText, DecryptionKeyShare, PublicKeyShare,
    SecretKeyShare, create_dk_share, decrypt_preimage, encrypt_preimage, verify_agg_dk,
    verify_ciphertext, verify_dk_share,
};

use crate::ln::ContractId;

#[derive(Debug, Clone, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub struct IncomingContract {
    pub commitment: Commitment,
    pub ciphertext: CipherText,
}

picomint_redb::consensus_value!(IncomingContract);

#[derive(Debug, Clone, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub struct Commitment {
    pub payment_hash: sha256::Hash,
    /// Invoice amount: what the LN payer paid the gateway.
    pub amount: Amount,
    /// Gateway's combined cut (LN routing + tx fee). The federation will
    /// credit the recipient `amount - fee` ecash on claim.
    pub fee: Amount,
    pub claim_pk: XOnlyPublicKey,
    pub refund_pk: XOnlyPublicKey,
    pub ephemeral_pk: PublicKey,
}

impl IncomingContract {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agg_pk: AggregatePublicKey,
        encryption_seed: [u8; 32],
        preimage: [u8; 32],
        payment_hash: sha256::Hash,
        amount: Amount,
        fee: Amount,
        claim_pk: XOnlyPublicKey,
        refund_pk: XOnlyPublicKey,
        ephemeral_pk: PublicKey,
    ) -> Self {
        let commitment = Commitment {
            payment_hash,
            amount,
            fee,
            claim_pk,
            refund_pk,
            ephemeral_pk,
        };

        let ciphertext = encrypt_preimage(
            &agg_pk,
            &encryption_seed,
            &preimage,
            &commitment.consensus_hash(),
        );

        Self {
            commitment,
            ciphertext,
        }
    }

    pub fn contract_id(&self) -> ContractId {
        ContractId(self.consensus_hash())
    }

    pub fn verify(&self) -> bool {
        verify_ciphertext(&self.ciphertext, &self.commitment.consensus_hash())
    }

    pub fn verify_decryption_share(
        &self,
        pk: &PublicKeyShare,
        dk_share: &DecryptionKeyShare,
    ) -> bool {
        verify_dk_share(
            pk,
            dk_share,
            &self.ciphertext,
            &self.commitment.consensus_hash(),
        )
    }

    pub fn verify_agg_decryption_key(
        &self,
        agg_pk: &AggregatePublicKey,
        agg_decryption_key: &AggregateDecryptionKey,
    ) -> bool {
        verify_agg_dk(
            agg_pk,
            agg_decryption_key,
            &self.ciphertext,
            &self.commitment.consensus_hash(),
        )
    }

    pub fn verify_preimage(&self, preimage: &[u8; 32]) -> bool {
        verify_preimage(&self.commitment.payment_hash, preimage)
    }

    pub fn decrypt_preimage(
        &self,
        agg_decryption_key: &AggregateDecryptionKey,
    ) -> Option<[u8; 32]> {
        let preimage = decrypt_preimage(&self.ciphertext, agg_decryption_key);

        if self.verify_preimage(&preimage) {
            Some(preimage)
        } else {
            None
        }
    }

    pub fn create_decryption_key_share(&self, sk: &SecretKeyShare) -> DecryptionKeyShare {
        create_dk_share(sk, &self.ciphertext)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub struct OutgoingContract {
    pub payment_hash: sha256::Hash,
    /// Invoice amount: what the gateway will pay over LN.
    pub amount: Amount,
    /// Gateway's combined cut (LN routing + tx fee). The client funds
    /// `amount + fee` so the gateway claims that on preimage delivery.
    pub fee: Amount,
    pub expiry: u64,
    pub claim_pk: XOnlyPublicKey,
    pub refund_pk: XOnlyPublicKey,
    pub tweak: [u8; 16],
}

picomint_redb::consensus_value!(OutgoingContract);

impl OutgoingContract {
    pub fn contract_id(&self) -> ContractId {
        ContractId(self.consensus_hash())
    }

    pub fn forfeit_message(&self) -> Message {
        Message::from_digest(*self.contract_id().0.as_ref())
    }

    pub fn verify_preimage(&self, preimage: &[u8; 32]) -> bool {
        verify_preimage(&self.payment_hash, preimage)
    }

    pub fn verify_forfeit_signature(&self, signature: &Signature) -> bool {
        secp256k1::global::SECP256K1
            .verify_schnorr(signature, &self.forfeit_message(), &self.claim_pk)
            .is_ok()
    }

    pub fn verify_gateway_response(&self, gateway_response: &Result<[u8; 32], Signature>) -> bool {
        match gateway_response {
            Ok(preimage) => self.verify_preimage(preimage),
            Err(signature) => self.verify_forfeit_signature(signature),
        }
    }

    pub fn verify_invoice_auth(&self, message: sha256::Hash, signature: &Signature) -> bool {
        secp256k1::global::SECP256K1
            .verify_schnorr(
                signature,
                &Message::from_digest(*message.as_ref()),
                &self.refund_pk,
            )
            .is_ok()
    }
}

fn verify_preimage(payment_hash: &sha256::Hash, preimage: &[u8; 32]) -> bool {
    preimage.consensus_hash::<sha256::Hash>() == *payment_hash
}

#[test]
fn test_verify_preimage() {
    use bitcoin::hashes::Hash;

    assert!(verify_preimage(
        &bitcoin::hashes::sha256::Hash::hash(&[42; 32]),
        &[42; 32]
    ));
}
