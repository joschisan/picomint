use crate::{Amount, OutPoint};
use bitcoin::hashes::sha256;
use bitcoin::secp256k1;
use picomint_encoding::{Decodable, Encodable};
use secp256k1::schnorr::Signature;
use secp256k1::{Keypair, Message, PublicKey, SecretKey, XOnlyPublicKey, ecdh};
use serde::{Deserialize, Serialize};
use tpe::{
    AggregateDecryptionKey, AggregatePublicKey, CipherText, DecryptionKeyShare, PublicKeyShare,
    SecretKeyShare, create_dk_share, decrypt_preimage, derive_agg_dk, encrypt_preimage,
    verify_agg_dk, verify_ciphertext, verify_dk_share,
};

use crate::ln::secret::IncomingContractSecret;
use crate::ln::{ContractId, OfferId};

/// What a recipient asks a gateway to fund: terms plus the preimage that
/// settles them, encrypted to the federation.
///
/// Authored end to end by the recipient, who derives every field from one
/// ECDH secret, and identified by [`Self::offer_id`]. The gateway cannot
/// alter it without invalidating the ciphertext, and does not need to: its
/// own stake is the refund key, which it attaches on funding to make an
/// [`IncomingContract`].
#[derive(Debug, Clone, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub struct IncomingOffer {
    pub commitment: Commitment,
    pub ciphertext: CipherText,
}

/// A funded [`IncomingOffer`]: what the federation holds and what
/// [`crate::ln::LightningInput::Incoming`] spends.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub struct IncomingContract {
    pub offer: IncomingOffer,
    /// Who is repaid when the preimage fails to decrypt — the gateway that
    /// funded the offer.
    ///
    /// Outside the offer, and so bound by neither the ciphertext nor
    /// [`IncomingOffer::offer_id`]. Nothing needs it to be: it is only
    /// consulted when decryption *fails*, and a recipient whose
    /// reconstruction matched the offer id will always decrypt. Keeping it
    /// out is what lets that recipient rebuild the offer without being told
    /// a key it never reads, which is 32 bytes off every entry of a stream
    /// every client walks. It is still covered by the transaction signature,
    /// since the txid hashes the outputs, so it cannot be malleated in
    /// flight.
    pub refund_pk: XOnlyPublicKey,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub struct Commitment {
    pub payment_hash: sha256::Hash,
    /// Invoice amount: what the LN payer paid the gateway.
    pub amount: Amount,
    /// Gateway's combined cut (LN routing + tx fee). The federation will
    /// credit the recipient `amount - fee` ecash on claim.
    pub fee: Amount,
    pub claim_pk: XOnlyPublicKey,
    pub ephemeral_pk: PublicKey,
}

impl IncomingOffer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agg_pk: AggregatePublicKey,
        encryption_seed: [u8; 32],
        preimage: [u8; 32],
        payment_hash: sha256::Hash,
        amount: Amount,
        fee: Amount,
        claim_pk: XOnlyPublicKey,
        ephemeral_pk: PublicKey,
    ) -> Self {
        let commitment = Commitment {
            payment_hash,
            amount,
            fee,
            claim_pk,
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

    /// Identity of the offer: the commitment and the ciphertext over it.
    ///
    /// Everything here the recipient derived itself, so it can compute this
    /// without being told any of it back.
    pub fn offer_id(&self) -> OfferId {
        OfferId((&self.commitment, &self.ciphertext).consensus_hash())
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

/// What a client needs from the incoming-contract stream, in place of the
/// whole [`IncomingContract`].
///
/// The stream is federation-wide — every client downloads every entry and
/// discards the ones that are not its own — so its size is paid by every
/// client for every payment anyone receives. Most of a contract is
/// redundant to the recipient: the ciphertext is a deterministic function
/// of a seed only they can derive, and the payment hash and claim key fall
/// out of the same ECDH. Only what cannot be derived travels here, plus a
/// payment hash to reject foreign entries cheaply and an offer id to prove
/// the rebuild — 153 bytes against the contract's 361.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub struct IncomingContractSummary {
    pub outpoint: OutPoint,
    /// Commitment to the preimage the recipient derives, and so the cheap
    /// half of matching: hashing a derived preimage costs nothing next to
    /// the BLS rebuild behind it, and a mismatch dismisses a foreign entry
    /// without ever reaching it.
    ///
    /// The *preimage* could not travel here — it is what settles the
    /// inbound HTLC, and the federation withholding it until the contract
    /// is claimed is what makes the swap atomic. Its hash is already public
    /// in the invoice.
    pub payment_hash: sha256::Hash,
    pub amount: Amount,
    pub fee: Amount,
    /// ECDH input — everything the recipient derives hangs off this.
    pub ephemeral_pk: PublicKey,
    /// Id of the offer the federation actually holds funded. A recipient
    /// rebuilds the offer from what it derived plus the fields above; a
    /// matching id proves the reconstruction is byte-identical to the stored
    /// one, and so that every check
    /// [`crate::ln::LightningInput::Incoming`] runs at claim time will pass.
    pub offer_id: OfferId,
}

impl IncomingContractSummary {
    pub fn new(outpoint: OutPoint, offer: &IncomingOffer) -> Self {
        Self {
            outpoint,
            payment_hash: offer.commitment.payment_hash,
            amount: offer.commitment.amount,
            fee: offer.commitment.fee,
            ephemeral_pk: offer.commitment.ephemeral_pk,
            offer_id: offer.offer_id(),
        }
    }

    /// Value the recipient is credited on a successful claim.
    pub fn claim_amount(&self) -> Option<Amount> {
        self.amount.checked_sub(self.fee)
    }

    /// Try to claim this entry with `sk`, `None` when it is not ours.
    ///
    /// Two stages. [`Self::payment_hash`] rejects a foreign entry for one
    /// ECDH and two hashes, which matters because the second stage is BLS
    /// and would otherwise be paid for every stranger's contract in the
    /// stream. On a hit the offer is rebuilt through [`IncomingOffer::new`]
    /// — the same constructor the recipient used to author it, so the two
    /// cannot drift apart — and checked against [`Self::offer_id`].
    ///
    /// The split also separates two signals: a payment-hash miss is someone
    /// else's contract, while a hit whose rebuild does not match the id
    /// means the summary disagrees with what the federation stores.
    pub fn recover(
        &self,
        agg_pk: &AggregatePublicKey,
        sk: &SecretKey,
    ) -> Option<(Keypair, AggregateDecryptionKey)> {
        let shared_secret = ecdh::SharedSecret::new(&self.ephemeral_pk, sk).secret_bytes();

        let contract_secret = IncomingContractSecret::new(shared_secret);

        let preimage = contract_secret.preimage();

        if preimage.consensus_hash::<sha256::Hash>() != self.payment_hash {
            return None;
        }

        let claim_keypair = sk
            .mul_tweak(&contract_secret.claim_tweak())
            .expect("Tweak is valid")
            .keypair(secp256k1::SECP256K1);

        let claim_pk = claim_keypair.x_only_public_key().0;
        let encryption_seed = contract_secret.encryption_seed();

        let rebuilt = IncomingOffer::new(
            *agg_pk,
            encryption_seed,
            preimage,
            self.payment_hash,
            self.amount,
            self.fee,
            claim_pk,
            self.ephemeral_pk,
        );

        if rebuilt.offer_id() != self.offer_id {
            return None;
        }

        Some((claim_keypair, derive_agg_dk(agg_pk, &encryption_seed)))
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
