use std::collections::BTreeMap;

use picomint_encoding::{Decodable, Encodable};
use serde::{Deserialize, Serialize};
use tbs::{AggregatePublicKey, PublicKeyShare, SecretKeyShare};

use crate::{Amount, NodeId};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapConfig {
    pub private: SwapConfigPrivate,
    pub consensus: SwapConfigConsensus,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize, Encodable, Decodable)]
pub struct SwapConfigConsensus {
    /// The key the mint attests funded receive contracts under: what a
    /// [`crate::swap::SwapAddress`] names and a send contract in another
    /// mint verifies against.
    pub agg_pk: AggregatePublicKey,
    pub pks: BTreeMap<NodeId, PublicKeyShare>,
    pub input_fee: Amount,
    pub output_fee: Amount,
}

#[derive(Debug, Clone, Serialize, Deserialize, Encodable, Decodable)]
pub struct SwapConfigPrivate {
    pub sk: SecretKeyShare,
}
