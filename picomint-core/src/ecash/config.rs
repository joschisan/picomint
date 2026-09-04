use std::collections::BTreeMap;

use picomint_encoding::{Decodable, Encodable};
use serde::{Deserialize, Serialize};
use tbs::{AggregatePublicKey, PublicKeyShare};

use crate::ecash::Denomination;
use crate::{Amount, PeerId};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ECashConfig {
    pub private: ECashConfigPrivate,
    pub consensus: ECashConfigConsensus,
}

pub fn consensus_denominations() -> impl DoubleEndedIterator<Item = Denomination> {
    (0..42).map(Denomination)
}

pub fn client_denominations() -> impl DoubleEndedIterator<Item = Denomination> + ExactSizeIterator {
    (9..42).map(Denomination)
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, Encodable, Decodable)]
pub struct ECashConfigConsensus {
    pub tbs_agg_pks: BTreeMap<Denomination, AggregatePublicKey>,
    pub tbs_pks: BTreeMap<Denomination, BTreeMap<PeerId, PublicKeyShare>>,
    pub input_fee: Amount,
    pub output_fee: Amount,
}

#[derive(Clone, Debug, Serialize, Deserialize, Encodable, Decodable)]
pub struct ECashConfigPrivate {
    pub tbs_sks: BTreeMap<Denomination, tbs::SecretKeyShare>,
}
