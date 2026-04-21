use std::collections::BTreeMap;

use iroh_base::PublicKey;
use serde::{Deserialize, Serialize};

use crate::PeerId;
use crate::config::FederationId;
use picomint_encoding::{Decodable, Encodable};

/// Everything a client needs to download the federation config and bootstrap.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Encodable, Decodable)]
pub struct InviteCode {
    pub federation_id: FederationId,
    pub peers: BTreeMap<PeerId, PublicKey>,
}

impl InviteCode {
    pub fn new(node_id: PublicKey, peer: PeerId, federation_id: FederationId) -> Self {
        Self {
            federation_id,
            peers: BTreeMap::from([(peer, node_id)]),
        }
    }
}

impl Serialize for InviteCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        picomint_base32::encode(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for InviteCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        picomint_base32::decode(&String::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}
