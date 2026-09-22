//! # Lightning Module
//!
//! This module lets a client pay and receive over the Lightning network
//! through a gateway it trusts: a sender relies on the gateway to answer
//! every payment with the preimage or a forfeit signature, a recipient
//! lets the gateway author the contract it funds, preimage included.

pub mod config;
pub mod contracts;
pub mod gateway;
pub mod lnurl;
pub mod methods;
pub mod secret;

use bitcoin::hashes::sha256;
use bitcoin::secp256k1::schnorr::Signature;
use lightning_invoice::Bolt11Invoice;
use picomint_encoding::{Decodable, Encodable};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::lightning::contracts::{IncomingContract, OutgoingContract};
use crate::{Amount, OutPoint};

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize, Decodable, Encodable)]
pub enum LightningInvoice {
    Bolt11(Bolt11Invoice),
}

impl LightningInvoice {
    /// Access the wrapped Bolt11 invoice. Single-variant for now; the
    /// getter exists so callers don't have to peel the enum.
    pub fn bolt11(&self) -> &Bolt11Invoice {
        match self {
            LightningInvoice::Bolt11(invoice) => invoice,
        }
    }
}

/// Minimum contract amount to ensure the incoming contract can be claimed
/// without additional funds.
pub const MINIMUM_INCOMING_CONTRACT_AMOUNT: Amount = Amount::from_sat(5);

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub struct ContractId(pub sha256::Hash);

#[derive(Debug, Clone, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub enum LightningInput {
    Outgoing(OutPoint, OutgoingWitness),
    Incoming(OutPoint),
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub enum OutgoingWitness {
    Claim([u8; 32]),
    Cancel(Signature),
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub enum LightningOutput {
    Outgoing(OutgoingContract),
    Incoming(IncomingContract),
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Error, Encodable, Decodable)]
pub enum LightningInputError {
    #[error("No contract found for given ContractId")]
    UnknownContract,
    #[error("The preimage is invalid")]
    InvalidPreimage,
    #[error("The forfeit signature is invalid")]
    InvalidForfeitSignature,
    #[error("Amount arithmetic overflowed u64 msat")]
    ArithmeticOverflow,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Error, Encodable, Decodable)]
pub enum LightningOutputError {
    #[error("Amount arithmetic overflowed u64 msat")]
    ArithmeticOverflow,
}
