//! Wire methods exposed at the top-level `Core` scope — no module prefix.
//!
//! Each method has a `Request` and a `Response` type. The [`CoreMethod`] enum
//! ties them together; variants carry the request payload, and the response
//! type for the variant `X` is `XResponse`.

use picomint_encoding::{Decodable, Encodable};

use crate::config::ConsensusConfig;
use crate::expiry::ExpiryStatus;
use crate::tx::{Transaction, TxError};

// ── config ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct ConfigRequest {
    /// Invite id of the invite code this download is for. The issuing guardian
    /// checks the registered expiration date and user limit and counts the
    /// download towards the limit; there is no way to fetch the config without
    /// a recognized invite.
    pub invite_id: [u8; 16],
}

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct ConfigResponse {
    pub config: ConsensusConfig,
}

// ── submit-transaction ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct SubmitTxRequest {
    pub tx: Transaction,
}

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct SubmitTxResponse {
    pub outcome: Result<(), TxError>,
}

// ── liveness ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct LivenessRequest;

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct LivenessResponse;

// ── expiry-status ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct ExpiryStatusRequest;

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct ExpiryStatusResponse {
    pub status: Option<ExpiryStatus>,
}

// ── dispatch enum ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub enum CoreMethod {
    Config(ConfigRequest),
    SubmitTx(SubmitTxRequest),
    Liveness(LivenessRequest),
    ExpiryStatus(ExpiryStatusRequest),
}
