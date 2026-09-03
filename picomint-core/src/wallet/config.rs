use std::collections::BTreeMap;

use crate::{Amount, PeerId};
use picomint_encoding::{Decodable, Encodable};
use serde::{Deserialize, Serialize};
use tss::{AggregatePublicKey, PublicKeyShare, SecretKeyShare};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletConfig {
    pub private: WalletConfigPrivate,
    pub consensus: WalletConfigConsensus,
}

#[derive(Clone, Debug, Serialize, Deserialize, Encodable, Decodable)]
pub struct WalletConfigPrivate {
    pub sks: SecretKeyShare,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, Encodable, Decodable)]
pub struct WalletConfigConsensus {
    /// The aggregate public key of the federation's taproot wallet
    pub agg_pk: AggregatePublicKey,
    /// The public key shares of the guardians
    pub pks: BTreeMap<PeerId, PublicKeyShare>,
    /// The minimum feerate doubles for each pending transaction in the stack,
    /// protecting against catastrophic feerate estimation errors
    pub feerate_base: u32,
    /// The minimum amount a user can send on chain
    pub dust_limit: bitcoin::Amount,
    /// Fee charged per wallet input
    pub input_fee: Amount,
    /// Fee charged per wallet output
    pub output_fee: Amount,
}

impl WalletConfigConsensus {
    pub fn new(agg_pk: AggregatePublicKey, pks: BTreeMap<PeerId, PublicKeyShare>) -> Self {
        Self {
            agg_pk,
            pks,
            // This is intentionally lower than the 1 sat/vB minimum feerate
            // vote floor. This allows for at least three pending transactions
            // which only pay the consensus feerate before the exponential
            // doubling kicks in.
            feerate_base: 250,
            dust_limit: bitcoin::Amount::from_sat(10_000),
            input_fee: crate::Amount::from_sat(10),
            output_fee: crate::Amount::from_sat(10),
        }
    }
}
