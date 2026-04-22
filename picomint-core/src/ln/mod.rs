//! # Lightning Module
//!
//! This module allows to atomically and trustlessly (in the federated trust
//! model) interact with the Lightning network through a Lightning gateway.

pub mod config;
pub mod contracts;
pub mod gateway_api;
pub mod lnurl;
pub mod methods;
pub mod routes;
pub mod secret;

use bitcoin::hashes::sha256;
use bitcoin::secp256k1::schnorr::Signature;
use lightning_invoice::Bolt11Invoice;
use picomint_encoding::{Decodable, Encodable};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tpe::AggregateDecryptionKey;

use crate::ln::contracts::{IncomingContract, OutgoingContract};
use crate::{Amount, OutPoint};

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum Bolt11InvoiceDescription {
    Direct(String),
    Hash(sha256::Hash),
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize, Decodable, Encodable)]
pub enum LightningInvoice {
    Bolt11(Bolt11Invoice),
}

/// Minimum contract amount to ensure the incoming contract can be claimed
/// without additional funds.
pub const MINIMUM_INCOMING_CONTRACT_AMOUNT: Amount = Amount::from_sats(5);

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub struct ContractId(pub sha256::Hash);

picomint_redb::consensus_key!(ContractId);

#[derive(Debug, Clone, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub enum LightningInput {
    Outgoing(OutPoint, OutgoingWitness),
    Incoming(OutPoint, AggregateDecryptionKey),
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Deserialize, Serialize, Encodable, Decodable)]
pub enum OutgoingWitness {
    Claim([u8; 32]),
    Refund,
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
    #[error("The contracts locktime has passed")]
    Expired,
    #[error("The contracts locktime has not yet passed")]
    NotExpired,
    #[error("The aggregate decryption key is invalid")]
    InvalidDecryptionKey,
    #[error("The forfeit signature is invalid")]
    InvalidForfeitSignature,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Error, Encodable, Decodable)]
pub enum LightningOutputError {
    #[error("The contract is invalid")]
    InvalidContract,
    #[error("The contract is expired")]
    ContractExpired,
}

#[derive(Debug, Clone, Hash, Eq, PartialEq, Encodable, Decodable, Serialize, Deserialize)]
pub enum LightningConsensusItem {
    BlockCountVote(u64),
    UnixTimeVote(u64),
}
