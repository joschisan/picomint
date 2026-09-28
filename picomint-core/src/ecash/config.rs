use std::collections::BTreeMap;

use picomint_encoding::{Decodable, Encodable};
use serde::{Deserialize, Serialize};
use tbs::{AggregatePublicKey, PublicKeyShare};

use crate::NodeId;
use crate::ecash::Denomination;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EcashConfig {
    pub private: EcashConfigPrivate,
    pub consensus: EcashConfigConsensus,
}

pub fn consensus_denominations() -> impl DoubleEndedIterator<Item = Denomination> {
    (0..42).map(Denomination)
}

pub fn client_denominations() -> impl DoubleEndedIterator<Item = Denomination> + ExactSizeIterator {
    (9..42).map(Denomination)
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, Encodable, Decodable)]
pub struct EcashConfigConsensus {
    pub tbs_agg_pks: BTreeMap<Denomination, AggregatePublicKey>,
    pub tbs_pks: BTreeMap<Denomination, BTreeMap<NodeId, PublicKeyShare>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Encodable, Decodable)]
pub struct EcashConfigPrivate {
    pub tbs_sks: BTreeMap<Denomination, tbs::SecretKeyShare>,
}
