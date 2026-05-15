//! Lightning module wire methods. Federation-side methods are framed by the
//! [`LnMethod`] enum; client↔gateway methods are framed by [`GatewayMethod`].
//! Each method has a `Request` and a `Response` type; on the wire, every call
//! returns `Result<Vec<u8>, String>` with the bytes being the response struct
//! consensus-encoded.

use bitcoin::hashes::sha256;
use bitcoin::secp256k1::schnorr::Signature;
use lightning_invoice::Bolt11Invoice;
use picomint_encoding::{Decodable, Encodable};
use tpe::DecryptionKeyShare;

use crate::OutPoint;
use crate::config::FederationId;
use crate::ln::ContractId;
use crate::ln::LightningInvoice;
use crate::ln::contracts::{IncomingContract, OutgoingContract};
use crate::ln::gateway::{GatewayInfo, GatewayPk};

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
    pub expiry: u64,
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

// ── outgoing-contract-expiry ────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct OutgoingContractExpiryRequest {
    pub outpoint: OutPoint,
}

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct OutgoingContractExpiryResponse {
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
    pub gateways: Vec<GatewayPk>,
}

// ── dispatch enum ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub enum LnMethod {
    ConsensusBlockCount(ConsensusBlockCountRequest),
    AwaitPreimage(AwaitPreimageRequest),
    DecryptionKeyShare(DecryptionKeyShareRequest),
    OutgoingContractExpiry(OutgoingContractExpiryRequest),
    AwaitIncomingContracts(AwaitIncomingContractsRequest),
    Gateways(GatewaysRequest),
}

// ── info ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct InfoRequest {
    pub federation: FederationId,
}

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct InfoResponse {
    pub info: Option<GatewayInfo>,
}

// ── send-payment ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct SendPaymentRequest {
    pub federation: FederationId,
    pub outpoint: OutPoint,
    pub contract: OutgoingContract,
    pub invoice: LightningInvoice,
    pub auth: Signature,
}

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct SendPaymentResponse {
    pub result: Result<[u8; 32], Signature>,
}

// ── create-invoice ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct CreateInvoiceRequest {
    pub federation: FederationId,
    pub contract: IncomingContract,
}

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct CreateInvoiceResponse {
    pub invoice: Bolt11Invoice,
}

// ── verify-preimage ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct VerifyPreimageRequest {
    pub hash: sha256::Hash,
    pub wait: bool,
}

/// LUD-21 verify response — gateway-internal iroh wire shape. The LNURL
/// daemon translates this to [`picomint_lnurl::VerifyResponse`] at the JSON
/// boundary it serves to external LNURL wallets.
#[derive(Debug, Clone, Encodable, Decodable, PartialEq, Eq)]
pub struct VerifyPreimageResponse {
    pub settled: bool,
    pub preimage: Option<[u8; 32]>,
}

// ── gateway dispatch enum ───────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub enum GatewayMethod {
    Info(InfoRequest),
    SendPayment(SendPaymentRequest),
    CreateInvoice(CreateInvoiceRequest),
    VerifyPreimage(VerifyPreimageRequest),
}
