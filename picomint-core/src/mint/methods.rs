//! Mint module wire methods.
//!
//! Each method has a `Request` and a `Response` type. The [`MintMethod`] enum
//! ties them together.

use picomint_encoding::{Decodable, Encodable};
use tbs::{BlindedMessage, BlindedSignatureShare};

use crate::TransactionId;
use crate::secp256k1::XOnlyPublicKey;

// ── signature-shares ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct SignatureSharesRequest {
    pub txid: TransactionId,
}

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct SignatureSharesResponse {
    pub shares: Vec<BlindedSignatureShare>,
}

// ── recovery-signature-shares ───────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct SignatureSharesRecoveryRequest {
    pub messages: Vec<BlindedMessage>,
}

/// Errors if the mint never signed one of `messages`. Recovery only asks
/// once [`IssuanceStateResponse`] has already confirmed every message, so a
/// miss here is a genuine fault rather than the expected outcome of probing.
#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct SignatureSharesRecoveryResponse {
    pub shares: Vec<BlindedSignatureShare>,
}

// ── issuance-state ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct IssuanceStateRequest {
    pub messages: Vec<BlindedMessage>,
}

/// `issued[i]` mirrors `messages[i]`. The membership half of a recovery scan:
/// the shares themselves are fetched once at the end, for the messages that
/// survived both this and [`SpendStateResponse`].
#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct IssuanceStateResponse {
    pub issued: Vec<bool>,
}

// ── spend-state ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct SpendStateRequest {
    pub nonces: Vec<XOnlyPublicKey>,
}

/// `spent[i]` mirrors `nonces[i]`. A recovery scan reads this first: a spent
/// nonce proves the counter was used without costing the client a blinded
/// message, which is the expensive half of a candidate.
#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct SpendStateResponse {
    pub spent: Vec<bool>,
}

// ── dispatch enum ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub enum MintMethod {
    SignatureShares(SignatureSharesRequest),
    SignatureSharesRecovery(SignatureSharesRecoveryRequest),
    SpendState(SpendStateRequest),
    IssuanceState(IssuanceStateRequest),
}
