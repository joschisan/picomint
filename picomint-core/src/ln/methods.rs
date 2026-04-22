//! Lightning module wire methods.
//!
//! Each method has a `Request` and a `Response` type. The [`LnMethod`] enum
//! ties them together.

use picomint_encoding::{Decodable, Encodable};
use tpe::DecryptionKeyShare;

use crate::OutPoint;
use crate::ln::ContractId;
use crate::ln::contracts::IncomingContract;

// ── consensus-block-count ───────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct ConsensusBlockCountRequest;

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct ConsensusBlockCountResponse {
    pub count: u64,
}

// ── await-preimage ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct AwaitPreimageRequest {
    pub outpoint: OutPoint,
    pub expiration: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct AwaitPreimageResponse {
    pub preimage: Option<[u8; 32]>,
}

// ── decryption-key-share ────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct DecryptionKeyShareRequest {
    pub outpoint: OutPoint,
}

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct DecryptionKeyShareResponse {
    pub share: DecryptionKeyShare,
}

// ── outgoing-contract-expiration ────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct OutgoingContractExpirationRequest {
    pub outpoint: OutPoint,
}

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct OutgoingContractExpirationResponse {
    pub contract: Option<(ContractId, u64)>,
}

// ── await-incoming-contracts ────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct AwaitIncomingContractsRequest {
    pub start: u64,
    pub batch: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct AwaitIncomingContractsResponse {
    pub contracts: Vec<(OutPoint, IncomingContract)>,
    pub next_index: u64,
}

// ── gateways ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct GatewaysRequest;

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct GatewaysResponse {
    pub gateways: Vec<String>,
}

// ── dispatch enum ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub enum LnMethod {
    ConsensusBlockCount(ConsensusBlockCountRequest),
    AwaitPreimage(AwaitPreimageRequest),
    DecryptionKeyShare(DecryptionKeyShareRequest),
    OutgoingContractExpiration(OutgoingContractExpirationRequest),
    AwaitIncomingContracts(AwaitIncomingContractsRequest),
    Gateways(GatewaysRequest),
}
