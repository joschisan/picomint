//! Mint module wire methods.
//!
//! Each method has a `Request` and a `Response` type. The [`MintMethod`] enum
//! ties them together.

use bitcoin::hashes::sha256;
use picomint_encoding::{Decodable, Encodable};
use tbs::{BlindedMessage, BlindedSignatureShare};

use crate::TransactionId;
use crate::mint::RecoveryItem;

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

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct SignatureSharesRecoveryResponse {
    pub shares: Vec<BlindedSignatureShare>,
}

// ── recovery-slice ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct RecoverySliceRequest {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct RecoverySliceResponse {
    pub items: Vec<RecoveryItem>,
}

// ── recovery-slice-hash ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct RecoverySliceHashRequest {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct RecoverySliceHashResponse {
    pub hash: sha256::Hash,
}

// ── recovery-count ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct RecoveryCountRequest;

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct RecoveryCountResponse {
    pub count: u64,
}

// ── dispatch enum ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub enum MintMethod {
    SignatureShares(SignatureSharesRequest),
    SignatureSharesRecovery(SignatureSharesRecoveryRequest),
    RecoverySlice(RecoverySliceRequest),
    RecoverySliceHash(RecoverySliceHashRequest),
    RecoveryCount(RecoveryCountRequest),
}
