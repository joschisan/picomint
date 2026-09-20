//! # Swap Module
//!
//! Pays an address in another mint through a broker, or in this mint
//! directly, with no timeout anywhere: the source mint locks the sender's
//! funds to the broker behind a threshold signature of the destination mint,
//! and the destination mint issues that signature over any receive contract
//! it holds. Each mint only ever reads its own state; the signature is what
//! crosses between them.

pub mod broker;
pub mod config;
pub mod methods;
pub mod secret;

use bitcoin::hashes::sha256;
use bitcoin::secp256k1;
use picomint_encoding::{Base32, Decodable, Encodable};
use secp256k1::{Keypair, PublicKey, SecretKey, XOnlyPublicKey, ecdh};
use serde::{Deserialize, Serialize};
use tbs::{AggregatePublicKey, Nonce, Signature};
use thiserror::Error;

use crate::config::MintId;
use crate::swap::secret::ReceiveContractSecret;
use crate::{Amount, OutPoint};

/// Minimum receive contract amount, so the claim can pay the mint's input
/// fee out of the contract itself.
pub const MINIMUM_RECEIVE_CONTRACT_AMOUNT: Amount = Amount::from_sat(5);

/// What a recipient hands out: the mint to receive in, its attestation
/// key, which a send contract in another mint locks to, and the static
/// key every payment derives its claim key from.
///
/// Nothing perishable goes in, so it stays valid for as long as the mint
/// exists. A sender that is itself a client of the named mint pays it
/// directly; any other sender pays it through a broker.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Encodable, Decodable, Base32)]
pub struct SwapAddress {
    pub mint: MintId,
    pub agg_pk: AggregatePublicKey,
    pub pk: PublicKey,
}

/// A payment to a [`SwapAddress`], as the destination mint holds it:
/// claimable by the key the sender derived from the address, findable by
/// the recipient through the ephemeral key it derived it with.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub struct ReceiveContract {
    pub amount: Amount,
    pub claim_pk: XOnlyPublicKey,
    pub ephemeral_pk: PublicKey,
}

impl ReceiveContract {
    /// Build the contract a sender pays to `address`, from a fresh ephemeral
    /// key. The same constructor the recipient rebuilds it with, so the two
    /// cannot drift apart.
    pub fn new(address: &SwapAddress, amount: Amount, ephemeral_keypair: &Keypair) -> Self {
        let shared_secret =
            ecdh::SharedSecret::new(&address.pk, &ephemeral_keypair.secret_key()).secret_bytes();

        let claim_pk = address
            .pk
            .mul_tweak(
                secp256k1::SECP256K1,
                &ReceiveContractSecret::new(shared_secret).claim_tweak(),
            )
            .expect("Tweak is valid")
            .x_only_public_key()
            .0;

        Self {
            amount,
            claim_pk,
            ephemeral_pk: ephemeral_keypair.public_key(),
        }
    }

    pub fn id(&self) -> ReceiveContractId {
        ReceiveContractId(self.consensus_hash())
    }
}

/// Identity of a [`ReceiveContract`], and what the destination mint attests:
/// a send contract names one, and the attestation over it is the send
/// contract's spending condition.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub struct ReceiveContractId(pub sha256::Hash);

impl ReceiveContractId {
    /// The point the destination mint's nodes sign: the id hashed to the
    /// curve the way a note nonce is, signed without a blinding key. The
    /// attestation key is the swap module's own, so the domain separation
    /// is in the key, not the hash.
    pub fn message(&self) -> Nonce {
        Nonce::from_public_key(*self.0.as_ref())
    }
}

/// What a recipient needs from the receive-contract stream, in place of the
/// whole [`ReceiveContract`]: the claim key is a function of the ephemeral
/// key and its own secret, so only the rest travels, plus the id to prove
/// the rebuild.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub struct ReceiveContractSummary {
    pub outpoint: OutPoint,
    pub amount: Amount,
    pub ephemeral_pk: PublicKey,
    pub id: ReceiveContractId,
}

impl ReceiveContractSummary {
    pub fn new(outpoint: OutPoint, contract: &ReceiveContract) -> Self {
        Self {
            outpoint,
            amount: contract.amount,
            ephemeral_pk: contract.ephemeral_pk,
            id: contract.id(),
        }
    }

    /// Try to claim this entry with `sk`, `None` when it is not ours. One
    /// ECDH, one tweak and one hash per entry; a matching id proves the
    /// rebuilt contract is byte-identical to the one the mint stores.
    pub fn recover(&self, sk: &SecretKey) -> Option<Keypair> {
        let shared_secret = ecdh::SharedSecret::new(&self.ephemeral_pk, sk).secret_bytes();

        let claim_keypair = sk
            .mul_tweak(&ReceiveContractSecret::new(shared_secret).claim_tweak())
            .expect("Tweak is valid")
            .keypair(secp256k1::SECP256K1);

        let rebuilt = ReceiveContract {
            amount: self.amount,
            claim_pk: claim_keypair.x_only_public_key().0,
            ephemeral_pk: self.ephemeral_pk,
        };

        (rebuilt.id() == self.id).then_some(claim_keypair)
    }
}

/// The sender's side of a swap, as the source mint holds it: `amount` locked
/// to the broker, released by the destination mint's attestation that a
/// contract with id `receive` is funded there. The broker's fee is the
/// difference to the receive contract's amount.
///
/// The source mint knows nothing of the destination beyond `agg_pk`: it is
/// a verifier of one signature, and both parties checked the key names the
/// right mint before anything was locked.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub struct SendContract {
    pub amount: Amount,
    pub receive: ReceiveContractId,
    pub agg_pk: AggregatePublicKey,
    pub claim_pk: XOnlyPublicKey,
}

impl SendContract {
    pub fn verify_attestation(&self, attestation: &Signature) -> bool {
        tbs::verify(self.receive.message(), *attestation, self.agg_pk)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub enum SwapInput {
    /// The broker takes a send contract with the destination's attestation.
    ClaimSend(OutPoint, Signature),
    /// The recipient takes a receive contract with its claim key.
    ClaimReceive(OutPoint),
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub enum SwapOutput {
    Send(SendContract),
    Receive(ReceiveContract),
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Error, Encodable, Decodable)]
pub enum SwapInputError {
    #[error("No contract found for given outpoint")]
    UnknownContract,
    #[error("The attestation is invalid")]
    InvalidAttestation,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Error, Encodable, Decodable)]
pub enum SwapOutputError {
    #[error("The receive contract amount is below the minimum")]
    AmountTooSmall,
}
