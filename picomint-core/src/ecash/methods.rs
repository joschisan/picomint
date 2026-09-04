//! ECash module wire methods.
//!
//! Each method has a `Request` and a `Response` type. The [`ECashMethod`] enum
//! ties them together.

use picomint_encoding::{Decodable, Encodable};
use tbs::{BlindedMessage, BlindedSignatureShare};

use crate::TransactionId;
use crate::ecash::Denomination;
use crate::secp256k1::XOnlyPublicKey;

// ── signature-shares ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct SignaturesRequest {
    pub txid: TransactionId,
}

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct SignaturesResponse {
    pub shares: Vec<BlindedSignatureShare>,
}

// ── restore-signature-shares ───────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct SignaturesRestoreRequest {
    pub messages: Vec<BlindedMessage>,
}

/// Errors if the mint never signed one of `messages`. Restore only asks
/// once [`IssuanceStateResponse`] has already confirmed every message, so a
/// miss here is a genuine fault rather than the expected outcome of probing.
#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct SignaturesRestoreResponse {
    pub shares: Vec<BlindedSignatureShare>,
}

// ── issuance-state ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct IssuanceStateRequest {
    pub messages: Vec<BlindedMessage>,
}

/// `issued[i]` mirrors `messages[i]`, carrying the denomination the mint
/// signed the message under, or `None` if it never signed it. The membership
/// half of a restore scan: the shares themselves are fetched once at the end,
/// for the messages that survived both this and [`SpendStateResponse`].
///
/// A client derives its nonces from a counter alone, so the denomination is
/// not recoverable from the seed and has to come back over the wire. It is
/// only a hint — the restored share is checked against that denomination's
/// aggregate public key, so a wrong answer fails verification rather than
/// crediting the wallet.
#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct IssuanceStateResponse {
    pub issued: Vec<Option<Denomination>>,
}

// ── spend-state ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct SpendStateRequest {
    pub nonces: Vec<XOnlyPublicKey>,
}

/// `spent[i]` mirrors `nonces[i]`. A restore scan reads this first: a spent
/// nonce proves the counter was used without costing the client a blinded
/// message, which is the expensive half of a candidate.
#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct SpendStateResponse {
    pub spent: Vec<bool>,
}

// ── dispatch enum ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub enum ECashMethod {
    Signatures(SignaturesRequest),
    SignaturesRestore(SignaturesRestoreRequest),
    SpendState(SpendStateRequest),
    IssuanceState(IssuanceStateRequest),
}
