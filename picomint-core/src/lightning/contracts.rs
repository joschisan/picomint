use crate::Amount;
use bitcoin::hashes::sha256;
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
/// Authored by whoever funds it — a gateway for an invoice, a sender for
/// a direct send — from the recipient's receive key through
/// [`Self::author`], which derives every field from one ECDH secret. The
/// recipient sees the contract once funded, in the mint's stream, and
/// recovers it from the ephemeral key. The funder holds the preimage, so
/// nothing about the contract can keep it from settling the payment, and
/// the contract has no refund branch.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub struct IncomingContract {
    pub payment_hash: sha256::Hash,
    /// Invoice amount: what the LN payer paid the gateway.
    pub amount: Amount,
    /// Gateway's combined cut (LN routing + tx fee). The mint will
    /// credit the recipient `amount - fee` ecash on claim.
    pub fee: Amount,
    pub claim_pk: XOnlyPublicKey,
    pub ephemeral_pk: PublicKey,
}

impl IncomingContract {
    /// Author a contract for `recipient_pk` from a fresh ephemeral key:
    /// the contract, and the preimage behind its payment hash. The
    /// recipient recovers the claim key from the ephemeral key through
    /// [`Self::recover`].
    pub fn author(recipient_pk: &PublicKey, amount: Amount, fee: Amount) -> (Self, [u8; 32]) {
        let ephemeral_kp = Keypair::new(secp256k1::SECP256K1, &mut rand::thread_rng());

        let shared_secret = ecdh::SharedSecret::new(recipient_pk, &ephemeral_kp.secret_key());

        let contract_secret = IncomingContractSecret::new(shared_secret.secret_bytes());

        let preimage = contract_secret.preimage();

        let claim_pk = recipient_pk
            .mul_tweak(secp256k1::SECP256K1, &contract_secret.claim_tweak())
            .expect("Tweak is valid")
            .x_only_public_key()
            .0;

        let contract = Self {
            payment_hash: preimage.consensus_hash(),
            amount,
            fee,
            claim_pk,
            ephemeral_pk: ephemeral_kp.public_key(),
        };

        (contract, preimage)
    }

    pub fn verify_preimage(&self, preimage: &[u8; 32]) -> bool {
        verify_preimage(&self.payment_hash, preimage)
    }

    /// Value the recipient is credited on a successful claim.
    pub fn claim_amount(&self) -> Option<Amount> {
        self.amount.checked_sub(self.fee)
    }

    /// The keypair that claims this contract if it is `sk`'s, `None` when
    /// it is not.
    ///
    /// Two stages. [`Self::payment_hash`] rejects a foreign entry for one
    /// ECDH and two hashes; a hit then derives the claim key the recipient
    /// locked the contract to and checks it against [`Self::claim_pk`].
    /// That split separates two signals: a payment-hash miss is someone
    /// else's contract, while a hit whose claim key does not match means
    /// the contract was not authored from this secret.
    pub fn recover(&self, sk: &SecretKey) -> Option<Keypair> {
        let shared_secret = ecdh::SharedSecret::new(&self.ephemeral_pk, sk).secret_bytes();

        let contract_secret = IncomingContractSecret::new(shared_secret);

        if !self.verify_preimage(&contract_secret.preimage()) {
            return None;
        }

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
