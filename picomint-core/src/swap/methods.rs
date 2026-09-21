//! Swap module wire methods. Mint-side methods are framed by the
//! [`SwapMethod`] enum; client↔broker methods are framed by [`BrokerMethod`].
//! Each method has a `Request` and a `Response` type; on the wire, every call
//! returns `Result<Vec<u8>, String>` with the bytes being the response struct
//! consensus-encoded.

use picomint_encoding::{Decodable, Encodable};
use tbs::{BlindedSignatureShare, Signature};

use crate::OutPoint;
use crate::config::MintId;
use crate::swap::broker::{BrokerInfo, BrokerPk};
use crate::swap::{ReceiveContract, ReceiveContractSummary, SendContract};

// ── attestation-share ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct AttestationShareRequest {
    pub outpoint: OutPoint,
}

/// This node's share of the attestation over the id of the receive
/// contract funded at the requested outpoint. A message signed in the
/// clear, so the share type is the blind scheme's with a blinding key of
/// one.
#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct AttestationShareResponse {
    pub share: BlindedSignatureShare,
}

// ── send-contract ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct SendContractRequest {
    pub outpoint: OutPoint,
}

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct SendContractResponse {
    pub contract: SendContract,
}

// ── await-receive-contracts ─────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct AwaitReceiveContractsRequest {
    pub start: u64,
    pub batch: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct AwaitReceiveContractsResponse {
    pub contracts: Vec<ReceiveContractSummary>,
    pub next_index: u64,
}

// ── brokers ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct BrokersRequest;

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct BrokersResponse {
    pub brokers: Vec<BrokerPk>,
}

// ── dispatch enum ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub enum SwapMethod {
    AttestationShare(AttestationShareRequest),
    SendContract(SendContractRequest),
    AwaitReceiveContracts(AwaitReceiveContractsRequest),
    Brokers(BrokersRequest),
}

// ── info ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct InfoRequest {
    pub mint: MintId,
}

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct InfoResponse {
    pub info: Option<BrokerInfo>,
}

// ── swap ────────────────────────────────────────────────────────────────────

/// Ask the broker to fund `receive` against the send contract locked to it
/// at `outpoint` in `mint`. The send contract itself is not carried: the
/// broker reads it from the mint, which is the copy that binds.
#[derive(Debug, Clone, Encodable, Decodable)]
pub struct SwapRequest {
    pub mint: MintId,
    pub outpoint: OutPoint,
    pub receive: ReceiveContract,
}

/// The destination mint's attestation over the receive contract: the
/// receipt, which the sender verifies against the key in its own send
/// contract.
#[derive(Debug, Clone, Encodable, Decodable)]
pub struct SwapResponse {
    pub attestation: Signature,
}

// ── broker dispatch enum ────────────────────────────────────────────────────

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Encodable, Decodable)]
pub enum BrokerMethod {
    Info(InfoRequest),
    Swap(SwapRequest),
}
