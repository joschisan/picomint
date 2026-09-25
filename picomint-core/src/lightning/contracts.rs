use crate::{Amount, OutPoint};
use bitcoin::hashes::{Hash, sha256};
use bitcoin::secp256k1;
use picomint_encoding::{Decodable, Encodable};
use secp256k1::schnorr::Signature;
use secp256k1::{Keypair, Message, PublicKey, SecretKey, XOnlyPublicKey, ecdh};
use serde::{Deserialize, Serialize};

use crate::lightning::ContractId;
use crate::lightning::secret::IncomingContractSecret;

/// What pays a recipient: the mint holds it until
/// [`crate::lightning::LightningInput::Incoming`] spends it.
///
/// Authored on the recipient's side — by its client or its lnurl daemon
/// for an invoice, by the sender for a direct send — from the
/// recipient's receive key through [`Self::author`], with a fresh
/// ephemeral key. The recipient recovers its claim key from the
/// ephemeral key when the funded contract shows up in the mint's stream.
///
/// The contract is its own preimage: [`Self::preimage`] is its hash and
/// [`Self::payment_hash`] the hash of that, so no hash rides in it and
/// whoever holds the contract can settle the payment. An invoice's
/// payment hash therefore names one contract, and the mint reporting it
/// funded means exactly that contract was. There is no refund branch.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub struct IncomingContract {
    /// Invoice amount: what the LN payer paid the gateway.
    pub amount: Amount,
    /// Gateway's combined cut (LN routing + tx fee). The mint will
    /// credit the recipient `amount - fee` ecash on claim.
    pub fee: Amount,
    pub claim_pk: XOnlyPublicKey,
    pub ephemeral_pk: PublicKey,
}

impl IncomingContract {
    /// Author a contract for `recipient_pk` from a fresh ephemeral key.
    /// The recipient recovers the claim key from the ephemeral key through
    /// [`Self::recover`].
    pub fn author(recipient_pk: &PublicKey, amount: Amount, fee: Amount) -> Self {
        let ephemeral_kp = Keypair::new(secp256k1::SECP256K1, &mut rand::thread_rng());

        let shared_secret = ecdh::SharedSecret::new(recipient_pk, &ephemeral_kp.secret_key());

        let contract_secret = IncomingContractSecret::new(shared_secret.secret_bytes());

        let claim_pk = recipient_pk
            .mul_tweak(secp256k1::SECP256K1, &contract_secret.claim_tweak())
            .expect("Tweak is valid")
            .x_only_public_key()
            .0;

        Self {
            amount,
            fee,
            claim_pk,
            ephemeral_pk: ephemeral_kp.public_key(),
        }
    }

    /// The preimage that settles the payment: the contract's own hash.
    pub fn preimage(&self) -> [u8; 32] {
        self.consensus_hash::<sha256::Hash>().to_byte_array()
    }

    /// The payment hash the invoice carries: the hash of the preimage.
    pub fn payment_hash(&self) -> sha256::Hash {
        self.preimage().consensus_hash()
    }

    /// Value the recipient is credited on a successful claim.
    pub fn claim_amount(&self) -> Option<Amount> {
        self.amount.checked_sub(self.fee)
    }

    /// The keypair that claims this contract if it is `sk`'s, `None` when
    /// it is not: the claim key the ECDH secret derives either is
    /// [`Self::claim_pk`] or the contract is someone else's.
    pub fn recover(&self, sk: &SecretKey) -> Option<Keypair> {
        let shared_secret = ecdh::SharedSecret::new(&self.ephemeral_pk, sk).secret_bytes();

        let contract_secret = IncomingContractSecret::new(shared_secret);

        let claim_keypair = sk
            .mul_tweak(&contract_secret.claim_tweak())
            .expect("Tweak is valid")
            .keypair(secp256k1::SECP256K1);

        if claim_keypair.x_only_public_key().0 != self.claim_pk {
            return None;
        }

        Some(claim_keypair)
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
    pub claim_pk: XOnlyPublicKey,
    /// Freshly generated per contract, and kept only by the sender's send
    /// state machine — nothing derives it, so nothing has to travel here to
    /// let the sender find it again.
    pub refund_pk: XOnlyPublicKey,
}

impl OutgoingContract {
    pub fn contract_id(&self) -> ContractId {
        ContractId(self.consensus_hash())
    }

    pub fn verify_preimage(&self, preimage: &[u8; 32]) -> bool {
        verify_preimage(&self.payment_hash, preimage)
    }

    /// Whether `signature` forfeits the funding of this contract at
    /// `outpoint`.
    pub fn verify_forfeit_signature(&self, outpoint: OutPoint, signature: &Signature) -> bool {
        secp256k1::global::SECP256K1
            .verify_schnorr(signature, &forfeit_message(outpoint), &self.claim_pk)
            .is_ok()
    }

    pub fn verify_gateway_response(
        &self,
        outpoint: OutPoint,
        gateway_response: &Result<[u8; 32], Signature>,
    ) -> bool {
        match gateway_response {
            Ok(preimage) => self.verify_preimage(preimage),
            Err(signature) => self.verify_forfeit_signature(outpoint, signature),
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

/// The message a gateway signs to forfeit one funding of an outgoing
/// contract: the funding's outpoint. The signature releases that funding
/// alone, so a contract funded twice needs two forfeits, and neither one
/// touches a funding the gateway is still paying for.
pub fn forfeit_message(outpoint: OutPoint) -> Message {
    Message::from_digest(outpoint.consensus_hash::<sha256::Hash>().to_byte_array())
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
