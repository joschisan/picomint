//! Gateway identity and pricing types — shared between clients and the
//! gateway daemon. Wire methods live in [`crate::ln::methods`].

use std::str::FromStr;

use bitcoin::secp256k1::XOnlyPublicKey;
use picomint_encoding::{Decodable, Encodable};
use serde::{Deserialize, Serialize};

use crate::Amount;

/// A gateway's identity — its iroh public key. `Serialize`, `Deserialize`,
/// and `FromStr` round-trip via [`picomint_base32`]; render with
/// `picomint_base32::encode`.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, PartialOrd, Ord, Encodable, Decodable)]
pub struct GatewayPk(pub iroh_base::PublicKey);

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
    /// The public key of the gateway's client module. Used to claim or
    /// cancel outgoing contracts and refund incoming contracts.
    pub module_public_key: XOnlyPublicKey,
    /// Fee the gateway charges on outgoing payments, the same whether it
    /// routes the payment over Lightning or settles it internally as the
    /// invoice's own issuer. Enforced exactly — the sender's contract must
    /// pay `send_fee` on top of the invoice amount. One flat price is what
    /// spares the sender from knowing how a payment will settle: nothing
    /// about the invoice changes what it costs.
    pub send_fee: PaymentFee,
    /// Fee the gateway charges on incoming payments. Enforced exactly —
    /// the incoming contract amount must equal `amount - receive_fee`.
    pub receive_fee: PaymentFee,
    /// Expiry delta in blocks for outgoing contracts. Sized for
    /// external LN sends (accounts for intermediate LN hops) and used for
    /// direct swaps as well.
    pub expiry_delta: u16,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize, Encodable, Decodable, Copy)]
pub struct PaymentFee {
    pub base: Amount,
    pub ppm: u64,
}

impl PaymentFee {
    /// Upper bound a client accepts on `GatewayInfo::send_fee`. Protects the
    /// sender against an abusive gateway's configured cut on outgoing
    /// payments; the gateway's own Lightning routing cost comes out of the
    /// flat fee, so it has to fit in here too.
    pub const SEND_FEE_LIMIT: Self = Self {
        base: Amount::from_sat(50),
        ppm: 10_000,
    };

    /// Upper bound a client accepts on `GatewayInfo::receive_fee`.
    pub const RECEIVE_FEE_LIMIT: Self = Self {
        base: Amount::from_sat(50),
        ppm: 10_000,
    };

    /// Whether both components are within `limit`. A derived `PartialOrd`
    /// would compare lexicographically — deciding on `base` alone unless the
    /// bases are equal — and wave through an over-limit `ppm`.
    pub fn is_within(&self, limit: &Self) -> bool {
        self.base <= limit.base && self.ppm <= limit.ppm
    }

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
