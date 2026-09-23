//! Lightning module wire methods. Mint-side methods are framed by the
//! [`LightningMethod`] enum; client↔gateway methods are framed by [`GatewayMethod`].
//! Each method has a `Request` and a `Response` type; on the wire, every call
//! returns `Result<Vec<u8>, String>` with the bytes being the response struct
//! consensus-encoded.

use bitcoin::hashes::sha256;
use bitcoin::secp256k1::schnorr::Signature;
use lightning_invoice::Bolt11Invoice;
use picomint_encoding::{Decodable, Encodable};

use crate::OutPoint;
use crate::config::MintId;
use crate::lightning::ContractId;
use crate::lightning::LightningInvoice;
use crate::lightning::contracts::{IncomingContract, OutgoingContract};
use crate::lightning::gateway::{GatewayInfo, GatewayPk};

// ── await-preimage ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct AwaitPreimageRequest {
    pub outpoint: OutPoint,
}

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct AwaitPreimageResponse {
    pub preimage: [u8; 32],
}

// ── await-outgoing-contract ─────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct AwaitOutgoingContractRequest {
    pub outpoint: OutPoint,
}

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct AwaitOutgoingContractResponse {
    pub contract: ContractId,
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

// ── incoming-payment ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct IncomingPaymentRequest {
    pub payment_hash: sha256::Hash,
}

/// The preimage of the funded incoming contract with the payment hash,
/// which is the proof of payment an LNURL wallet asks for, or `None`
/// while the mint holds no such contract.
#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct IncomingPaymentResponse {
    pub preimage: Option<[u8; 32]>,
}

// ── await-incoming-payment ──────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct AwaitIncomingPaymentRequest {
    pub payment_hash: sha256::Hash,
}

/// Returned once the mint holds a funded incoming contract with the
/// payment hash: its preimage.
#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct AwaitIncomingPaymentResponse {
    pub preimage: [u8; 32],
}

// ── gateways ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct GatewaysRequest;

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct GatewaysResponse {
    pub gateways: Vec<GatewayPk>,
}

// ── dispatch enum ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub enum LightningMethod {
    AwaitPreimage(AwaitPreimageRequest),
    AwaitOutgoingContract(AwaitOutgoingContractRequest),
    AwaitIncomingContracts(AwaitIncomingContractsRequest),
    IncomingPayment(IncomingPaymentRequest),
    AwaitIncomingPayment(AwaitIncomingPaymentRequest),
    Gateways(GatewaysRequest),
}

// ── info ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct InfoRequest {
    pub mint: MintId,
}

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct InfoResponse {
    pub info: Option<GatewayInfo>,
}

// ── send ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct SendRequest {
    pub mint: MintId,
    pub outpoint: OutPoint,
    pub contract: OutgoingContract,
    pub invoice: LightningInvoice,
    pub auth: Signature,
}

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct SendResponse {
    pub result: Result<[u8; 32], Signature>,
}

// ── receive ─────────────────────────────────────────────────────────────────

/// What a recipient asks a gateway to fund. The recipient authors the
/// contract, so it knows the payment hash the invoice has to carry, and
/// the mint reporting that hash funded means this contract and no other
/// was.
#[derive(Debug, Clone, Encodable, Decodable)]
pub struct ReceiveRequest {
    pub mint: MintId,
    pub contract: IncomingContract,
}

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct ReceiveResponse {
    pub invoice: Bolt11Invoice,
}

// ── gateway dispatch enum ───────────────────────────────────────────────────

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Encodable, Decodable)]
pub enum GatewayMethod {
    Info(InfoRequest),
    Send(SendRequest),
    Receive(ReceiveRequest),
}
