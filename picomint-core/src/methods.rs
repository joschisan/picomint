//! Wire methods exposed at the top-level `Core` scope — no module prefix.
//!
//! Each method has a `Request` and a `Response` type. The [`CoreMethod`] enum
//! ties them together; variants carry the request payload, and the response
//! type for the variant `X` is `XResponse`.

use picomint_encoding::{Decodable, Encodable};

use crate::NodeId;
use crate::config::ConsensusConfig;
use crate::config::{MintId, NodeEndpoint};
use crate::expiry::ExpiryStatus;
use crate::tx::{Transaction, TxError};
use std::collections::BTreeMap;

// ── config ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct ConfigRequest {
    /// Invite id of the invite code this download is for. The issuing node
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

// ── block-count ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct BlockCountRequest;

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct BlockCountResponse {
    pub count: u32,
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

// ── mint-info ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct MintInfoRequest;

/// The mint's identity and node set. Ungated, unlike [`ConfigRequest`]:
/// any joined client already holds both, and a caller that received them out
/// of band can pin them against a hash, so serving them grants nothing an
/// invite would otherwise gate.
#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct MintInfoResponse {
    pub mint: MintId,
    pub nodes: BTreeMap<NodeId, NodeEndpoint>,
}

impl MintInfoResponse {
    /// Built on both sides — by a node to answer, and by a client to
    /// commit to the answer it expects — so the two hash the same bytes.
    pub fn new(config: &ConsensusConfig) -> Self {
        Self {
            mint: config.calculate_mint_id(),
            nodes: config.nodes.clone(),
        }
    }
}

// ── dispatch enum ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub enum CoreMethod {
    Config(ConfigRequest),
    SubmitTx(SubmitTxRequest),
    BlockCount(BlockCountRequest),
    Liveness(LivenessRequest),
    ExpiryStatus(ExpiryStatusRequest),
    MintInfo(MintInfoRequest),
}
