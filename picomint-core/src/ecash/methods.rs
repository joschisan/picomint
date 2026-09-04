//! Ecash module wire methods.
//!
//! Each method has a `Request` and a `Response` type. The [`EcashMethod`] enum
//! ties them together.

use picomint_encoding::{Decodable, Encodable};
use tbs::{BlindedNonce, BlindedSignatureShare};

use crate::TransactionId;
use crate::ecash::Denomination;
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

// ── restore-signature-shares ───────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct SignatureSharesRestoreRequest {
    pub nonces: Vec<BlindedNonce>,
}

/// Errors if the mint never signed one of `nonces`. Restore only asks
/// once [`IssuanceStateResponse`] has already confirmed every nonce, so a
/// miss here is a genuine fault rather than the expected outcome of probing.
#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct SignatureSharesRestoreResponse {
    pub shares: Vec<BlindedSignatureShare>,
}

// ── issuance-state ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct IssuanceStateRequest {
    pub nonces: Vec<BlindedNonce>,
}

/// `issued[i]` mirrors `nonces[i]`, carrying the denomination the mint
/// signed the nonce under, or `None` if it never signed it. The membership
/// half of a restore scan: the shares themselves are fetched once at the end,
/// for the nonces that survived both this and [`SpendStateResponse`].
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
/// nonce, which is the expensive half of a candidate.
#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct SpendStateResponse {
    pub spent: Vec<bool>,
}

// ── dispatch enum ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub enum EcashMethod {
    SignatureShares(SignatureSharesRequest),
    SignatureSharesRestore(SignatureSharesRestoreRequest),
    SpendState(SpendStateRequest),
    IssuanceState(IssuanceStateRequest),
}
