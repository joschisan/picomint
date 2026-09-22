//! Lightning module wire methods. Mint-side methods are framed by the
//! [`LightningMethod`] enum; client↔gateway methods are framed by [`GatewayMethod`].
//! Each method has a `Request` and a `Response` type; on the wire, every call
//! returns `Result<Vec<u8>, String>` with the bytes being the response struct
//! consensus-encoded.

use bitcoin::hashes::sha256;
use bitcoin::secp256k1::PublicKey;
use bitcoin::secp256k1::schnorr::Signature;
use lightning_invoice::Bolt11Invoice;
use picomint_encoding::{Decodable, Encodable};

use crate::config::MintId;
use crate::lightning::ContractId;
use crate::lightning::LightningInvoice;
use crate::lightning::contracts::{IncomingContract, OutgoingContract};
use crate::lightning::gateway::{GatewayInfo, GatewayPk};
use crate::{Amount, OutPoint};

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

/// What a recipient needs to say to be paid: the mint, its receive key
/// and the amount. The gateway authors the contract, preimage included,
/// from the key; the recipient sees the contract once it is funded, in
/// the mint's stream, and recovers it from the same key.
#[derive(Debug, Clone, Encodable, Decodable)]
pub struct ReceiveRequest {
    pub mint: MintId,
    pub recipient: PublicKey,
    pub amount: Amount,
}

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct ReceiveResponse {
    pub invoice: Bolt11Invoice,
}

// ── verify ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct VerifyRequest {
    pub hash: sha256::Hash,
    pub wait: bool,
}

/// LUD-21 verify response — gateway-internal iroh wire shape. The LNURL
/// daemon translates this to [`picomint_lnurl::VerifyResponse`] at the JSON
/// boundary it serves to external LNURL wallets.
#[derive(Debug, Clone, Encodable, Decodable, PartialEq, Eq)]
pub struct VerifyResponse {
    pub settled: bool,
    pub preimage: Option<[u8; 32]>,
}

// ── gateway dispatch enum ───────────────────────────────────────────────────

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Encodable, Decodable)]
pub enum GatewayMethod {
    Info(InfoRequest),
    Send(SendRequest),
    Receive(ReceiveRequest),
    Verify(VerifyRequest),
}
