use std::collections::BTreeMap;

use crate::NodeId;
use picomint_encoding::{Decodable, Encodable};
use serde::{Deserialize, Serialize};
use tss::{AggregatePublicKey, PublicKeyShare, SecretKeyShare};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OnchainConfig {
    pub private: OnchainConfigPrivate,
    pub consensus: OnchainConfigConsensus,
}

#[derive(Clone, Debug, Serialize, Deserialize, Encodable, Decodable)]
pub struct OnchainConfigPrivate {
    pub sks: SecretKeyShare,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, Encodable, Decodable)]
pub struct OnchainConfigConsensus {
    /// The aggregate public key of the mint's taproot wallet
    pub agg_pk: AggregatePublicKey,
    /// The public key shares of the nodes
    pub pks: BTreeMap<NodeId, PublicKeyShare>,
}
