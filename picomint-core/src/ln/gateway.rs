//! Gateway identity and pricing types — shared between clients and the
//! gateway daemon. Wire methods live in [`crate::ln::methods`].

use std::ops::Add;
use std::str::FromStr;

use bitcoin::secp256k1::{PublicKey, XOnlyPublicKey};
use picomint_encoding::{Decodable, Encodable};
use serde::{Deserialize, Serialize};

use crate::Amount;

/// A gateway's identity — its iroh public key. `Serialize`, `Deserialize`,
/// and `FromStr` round-trip via [`picomint_base32`]; render with
/// `picomint_base32::encode`.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, PartialOrd, Ord, Encodable, Decodable)]
pub struct GatewayPk(pub iroh_base::PublicKey);

picomint_redb::consensus_key!(GatewayPk);

impl Serialize for GatewayPk {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        picomint_base32::encode(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for GatewayPk {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        picomint_base32::decode(&String::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

impl FromStr for GatewayPk {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        picomint_base32::decode(s)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize, Encodable, Decodable)]
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
    /// Expiry delta in blocks for outgoing contracts. Sized for
    /// external LN sends (accounts for intermediate LN hops) and used for
    /// direct swaps as well.
    pub expiry_delta: u64,
}

picomint_redb::consensus_value!(GatewayInfo);

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
        base: Amount::from_sat(50),
        ppm: 5_000,
    };

    /// Upper bound a client accepts on `GatewayInfo::receive_fee`.
    pub const RECEIVE_FEE_LIMIT: Self = Self {
        base: Amount::from_sat(50),
        ppm: 5_000,
    };

    /// Upper bound a client accepts on `GatewayInfo::ln_fee` — the LN
    /// routing headroom the gateway is allowed to charge.
    pub const LN_FEE_LIMIT: Self = Self {
        base: Amount::from_sat(100),
        ppm: 15_000,
    };

    pub fn add_to(&self, msat: u64) -> Amount {
        Amount::from_msat(msat.saturating_add(self.absolute_fee(msat)))
    }

    pub fn subtract_from(&self, msat: u64) -> Amount {
        Amount::from_msat(msat.saturating_sub(self.absolute_fee(msat)))
    }

    pub fn fee(&self, msat: u64) -> Amount {
        Amount::from_msat(self.absolute_fee(msat))
    }

    fn absolute_fee(&self, msat: u64) -> u64 {
        msat.saturating_mul(self.ppm)
            .saturating_div(1_000_000)
            .checked_add(self.base.msat)
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
