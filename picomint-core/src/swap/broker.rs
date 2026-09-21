//! Broker identity and pricing types — shared between clients and the
//! broker daemon. Wire methods live in [`crate::swap::methods`].

use bitcoin::secp256k1::XOnlyPublicKey;
use picomint_encoding::{Base32, Decodable, Encodable};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::lightning::gateway::PaymentFee;

/// A broker's identity, its iroh public key. Mint nodes recommend a broker
/// by it and clients dial it by it.
#[derive(
    Debug, Clone, Copy, Eq, PartialEq, Hash, PartialOrd, Ord, Encodable, Decodable, Base32,
)]
pub struct BrokerPk(pub iroh_base::PublicKey);

/// What a client needs to price and lock a swap through one broker on one
/// mint, as the broker announces it when probed.
#[derive(
    Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize, Encodable, Decodable, JsonSchema,
)]
pub struct BrokerInfo {
    /// The key the broker claims send contracts on this mint with, hex
    /// x-only.
    #[schemars(with = "String")]
    pub claim_pk: XOnlyPublicKey,
    /// Fee the broker charges on a swap out of this mint. Enforced exactly —
    /// the send contract must lock `fee` on top of the receive amount.
    pub fee: PaymentFee,
}

impl BrokerInfo {
    /// Upper bound a client accepts on [`BrokerInfo::fee`].
    pub const FEE_LIMIT: PaymentFee = PaymentFee {
        base: crate::Amount::from_sat(50),
        ppm: 10_000,
    };
}
