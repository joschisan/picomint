//! Wire types shared between picomint clients and the gateway daemon.
//! The HTTP request helpers themselves live client-side
//! (`picomint_client::ln::gateway_http`).

use std::ops::Add;

use bitcoin::secp256k1::schnorr::Signature;
use bitcoin::secp256k1::{PublicKey, XOnlyPublicKey};
use picomint_encoding::{Decodable, Encodable};
use serde::{Deserialize, Serialize};

use crate::Amount;
use crate::OutPoint;
use crate::config::FederationId;
use crate::ln::contracts::{IncomingContract, OutgoingContract};
use crate::ln::{Bolt11InvoiceDescription, LightningInvoice};

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateBolt11InvoicePayload {
    pub federation_id: FederationId,
    pub contract: IncomingContract,
    pub amount: Amount,
    pub description: Bolt11InvoiceDescription,
    pub expiry_secs: u32,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct SendPaymentPayload {
    pub federation_id: FederationId,
    pub outpoint: OutPoint,
    pub contract: OutgoingContract,
    pub invoice: LightningInvoice,
    pub auth: Signature,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct GatewayInfo {
    /// The public key of the gateway's lightning node. Signs the gateway's
    /// invoices so the sender can detect direct swaps by comparing against
    /// the invoice's recovered payee pubkey.
    pub lightning_public_key: PublicKey,
    /// The public key of the gateway's client module. Used to claim or
    /// cancel outgoing contracts and refund incoming contracts.
    pub module_public_key: XOnlyPublicKey,
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
