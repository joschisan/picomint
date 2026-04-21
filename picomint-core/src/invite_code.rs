use std::collections::BTreeMap;

use iroh_base::PublicKey;
use serde::{Deserialize, Serialize};

use crate::PeerId;
use crate::config::FederationId;
use picomint_encoding::{Decodable, Encodable};

/// Can be used to download the configs and bootstrap a client.
#[derive(Clone, Debug, Eq, PartialEq, Encodable, Decodable, Hash, Ord, PartialOrd)]
pub struct InviteCode(Vec<InviteCodePart>);

impl InviteCode {
    pub fn new(node_id: PublicKey, peer: PeerId, federation_id: FederationId) -> Self {
        Self(vec![
            InviteCodePart::FederationId(federation_id),
            InviteCodePart::Peer { peer, node_id },
        ])
    }

    /// Get all peer node ids in the [`InviteCode`].
    pub fn peers(&self) -> BTreeMap<PeerId, PublicKey> {
        self.0
            .iter()
            .filter_map(|entry| match entry {
                InviteCodePart::Peer { peer, node_id } => Some((*peer, *node_id)),
                InviteCodePart::FederationId(_) => None,
            })
            .collect()
    }

    /// Returns the federation's ID that can be used to authenticate the config
    /// downloaded from the API.
    pub fn federation_id(&self) -> Option<FederationId> {
        self.0.iter().find_map(|data| match data {
            InviteCodePart::FederationId(federation_id) => Some(*federation_id),
            InviteCodePart::Peer { .. } => None,
        })
    }
}

/// For extendability [`InviteCode`] consists of parts, where client can ignore
/// ones they don't understand.
#[derive(Clone, Debug, Eq, PartialEq, Encodable, Decodable, Hash, Ord, PartialOrd)]
enum InviteCodePart {
    /// Authentication id for the federation
    FederationId(FederationId),
    /// API endpoint of one of the guardians
    Peer {
        /// Peer id of the host from the node id
        peer: PeerId,
        /// Iroh public key of the peer's API endpoint
        node_id: PublicKey,
    },
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
