//! Wire methods exposed at the top-level `Core` scope — no module prefix.
//!
//! Each method has a `Request` and a `Response` type. The [`CoreMethod`] enum
//! ties them together; variants carry the request payload, and the response
//! type for the variant `X` is `XResponse`.

use picomint_encoding::{Decodable, Encodable};

use crate::config::ConsensusConfig;
use crate::transaction::{Transaction, TransactionError};

// ── config ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct ConfigRequest;

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct ConfigResponse {
    pub config: ConsensusConfig,
}

// ── submit-transaction ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct SubmitTransactionRequest {
    pub transaction: Transaction,
}

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct SubmitTransactionResponse {
    pub outcome: Result<(), TransactionError>,
}

// ── liveness ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct LivenessRequest;

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct LivenessResponse;

// ── dispatch enum ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub enum CoreMethod {
    Config(ConfigRequest),
    SubmitTransaction(SubmitTransactionRequest),
    Liveness(LivenessRequest),
}
