//! Wire types shared between picomint clients and the gateway daemon.
//! Requests and responses are length-framed, consensus-encoded, and sent
//! over iroh bi-streams (see `picomint_client::ln::gateway_api` for the
//! client-side dispatcher).
//!
//! Mirrors the federation's [`crate::module::Method`] shape: one typed
//! enum whose variants carry concrete `XRequest` structs; responses are
//! `XResponse` newtypes. The gateway has no modules so the enum is flat
//! rather than per-module nested.

use std::ops::Add;

use bitcoin::secp256k1::PublicKey;
use bitcoin::secp256k1::schnorr::Signature;
use lightning_invoice::Bolt11Invoice;
use picomint_encoding::{Decodable, Encodable};
use serde::{Deserialize, Serialize};

use crate::Amount;
use crate::OutPoint;
use crate::config::FederationId;
use crate::ln::contracts::{IncomingContract, OutgoingContract};
use crate::ln::{Bolt11InvoiceDescription, LightningInvoice};

/// Conservative cap for both requests and responses on the gateway wire.
/// All four methods have small, bounded payloads (invoices, contracts,
/// payment hashes) — 100 KiB leaves headroom without allowing abuse.
pub const GATEWAY_MAX_MESSAGE_BYTES: usize = 100_000;

// ── routing-info ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct GatewayInfoRequest {
    pub federation_id: FederationId,
}

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct GatewayInfoResponse {
    pub gateway_info: GatewayInfo,
}

// ── send-payment ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize, Encodable, Decodable)]
pub struct SendPaymentRequest {
    pub federation_id: FederationId,
    pub outpoint: OutPoint,
    pub contract: OutgoingContract,
    pub invoice: LightningInvoice,
    pub auth: Signature,
}

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct SendPaymentResponse {
    /// `Ok(preimage)` on successful payment, `Err(forfeit_sig)` when the
    /// gateway abandons the payment and signs over its claim.
    pub outcome: Result<[u8; 32], Signature>,
}

// ── create-bolt11-invoice ───────────────────────────────────────────────────

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize, Encodable, Decodable)]
pub struct CreateBolt11InvoiceRequest {
    pub federation_id: FederationId,
    pub contract: IncomingContract,
    pub amount: Amount,
    pub description: Bolt11InvoiceDescription,
    pub expiry_secs: u32,
}

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct CreateBolt11InvoiceResponse {
    pub invoice: Bolt11Invoice,
}

// ── verify-bolt11-preimage ──────────────────────────────────────────────────

#[derive(Debug, Clone, Eq, PartialEq, Hash, Encodable, Decodable)]
pub struct VerifyBolt11PreimageRequest {
    pub payment_hash: bitcoin::hashes::sha256::Hash,
    /// When true, the handler long-polls until the payment settles (or
    /// the underlying await resolves with a terminal state). When false,
    /// the handler returns the current state with a short internal
    /// timeout so callers can poll cheaply.
    pub wait: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Encodable, Decodable)]
pub struct VerifyBolt11PreimageResponse {
    pub settled: bool,
    pub preimage: Option<[u8; 32]>,
}

// ── dispatch enum ───────────────────────────────────────────────────────────

/// The wire method dispatched to a gateway over iroh. Each variant
/// carries the concrete request for the method; the response type for
/// variant `X` is `XResponse`.
#[derive(Debug, Clone, Encodable, Decodable)]
pub enum GatewayMethod {
    GatewayInfo(GatewayInfoRequest),
    SendPayment(SendPaymentRequest),
    CreateBolt11Invoice(CreateBolt11InvoiceRequest),
    VerifyBolt11Preimage(VerifyBolt11PreimageRequest),
}

// ── routing-info payload (public API, not wire-internal) ────────────────────

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize, Encodable, Decodable)]
pub struct GatewayInfo {
    /// The public key of the gateway's lightning node. Signs the gateway's
    /// invoices so the sender can detect direct swaps by comparing against
    /// the invoice's recovered payee pubkey.
    pub lightning_public_key: PublicKey,
    /// The public key of the gateway's client module. Used to claim or
    /// cancel outgoing contracts and refund incoming contracts.
    pub module_public_key: PublicKey,
    /// Fee the gateway charges on outgoing payments. Enforced exactly —
    /// the sender's contract must pay `send_fee` on top of the invoice
    /// amount for direct swaps, and `send_fee + ln_fee` for external LN.
    pub send_fee: PaymentFee,
    /// Fee the gateway charges on incoming payments. Enforced exactly —
    /// the incoming contract amount must equal `amount - receive_fee`.
    pub receive_fee: PaymentFee,
    /// Maximum Lightning routing cost the gateway is willing to absorb on
    /// external outgoing payments. Used by the gateway as LDK's
    /// `max_total_routing_fee_msat` cap and charged to the sender on top of
    /// `send_fee`.
    pub ln_fee: PaymentFee,
    /// Expiration delta in blocks for outgoing contracts. Sized for
    /// external LN sends (accounts for intermediate LN hops) and used for
    /// direct swaps as well.
    pub expiration_delta: u64,
}

#[derive(
    Debug,
    Clone,
    Eq,
    PartialEq,
    PartialOrd,
    Hash,
    Serialize,
    Deserialize,
    Encodable,
    Decodable,
    Copy,
)]
pub struct PaymentFee {
    pub base: Amount,
    pub ppm: u64,
}

impl PaymentFee {
    /// Upper bound a client accepts on `GatewayInfo::send_fee`. Protects the
    /// sender against an abusive gateway's configured tx cut on outgoing
    /// payments.
    pub const SEND_FEE_LIMIT: Self = Self {
        base: Amount::from_sats(50),
        ppm: 5_000,
    };

    /// Upper bound a client accepts on `GatewayInfo::receive_fee`.
    pub const RECEIVE_FEE_LIMIT: Self = Self {
        base: Amount::from_sats(50),
        ppm: 5_000,
    };

    /// Upper bound a client accepts on `GatewayInfo::ln_fee` — the LN
    /// routing headroom the gateway is allowed to charge.
    pub const LN_FEE_LIMIT: Self = Self {
        base: Amount::from_sats(100),
        ppm: 15_000,
    };

    pub fn add_to(&self, msats: u64) -> Amount {
        Amount::from_msats(msats.saturating_add(self.absolute_fee(msats)))
    }

    pub fn subtract_from(&self, msats: u64) -> Amount {
        Amount::from_msats(msats.saturating_sub(self.absolute_fee(msats)))
    }

    pub fn fee(&self, msats: u64) -> Amount {
        Amount::from_msats(self.absolute_fee(msats))
    }

    fn absolute_fee(&self, msats: u64) -> u64 {
        msats
            .saturating_mul(self.ppm)
            .saturating_div(1_000_000)
            .checked_add(self.base.msats)
            .expect("The division creates sufficient headroom to add the base fee")
    }
}

impl Add for PaymentFee {
    type Output = Self;
    fn add(self, rhs: Self) -> Self::Output {
        Self {
            base: self.base + rhs.base,
            ppm: self.ppm + rhs.ppm,
        }
    }
}
