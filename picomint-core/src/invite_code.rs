use std::collections::BTreeMap;
use std::io::Read;

use iroh_base::PublicKey;
use serde::{Deserialize, Serialize};

use crate::config::FederationId;
use crate::{NumPeersExt, PeerId};
use picomint_encoding::{Decodable, Encodable};

/// Information required for client to join Federation
///
/// Can be used to download the configs and bootstrap a client.
///
/// ## Invariants
/// Constructors have to guarantee that:
///   * At least one Api entry is present
///   * At least one Federation ID is present
#[derive(Clone, Debug, Eq, PartialEq, Encodable, Hash, Ord, PartialOrd)]
pub struct InviteCode(Vec<InviteCodePart>);

impl Decodable for InviteCode {
    fn consensus_decode<R: Read>(r: &mut R) -> std::io::Result<Self> {
        let inner: Vec<InviteCodePart> = Decodable::consensus_decode(r)?;

        if !inner
            .iter()
            .any(|data| matches!(data, InviteCodePart::Api { .. }))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "No API was provided in the invite code",
            ));
        }

        if !inner
            .iter()
            .any(|data| matches!(data, InviteCodePart::FederationId(_)))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "No Federation ID provided in invite code",
            ));
        }

        Ok(Self(inner))
    }
}

impl InviteCode {
    pub fn new(node_id: PublicKey, peer: PeerId, federation_id: FederationId) -> Self {
        Self(vec![
            InviteCodePart::Api { node_id, peer },
            InviteCodePart::FederationId(federation_id),
        ])
    }

    /// Constructs an [`InviteCode`] which contains as many guardian node ids
    /// as needed to always be able to join a working federation.
    pub fn new_with_essential_num_guardians(
        peers: &BTreeMap<PeerId, PublicKey>,
        federation_id: FederationId,
    ) -> Self {
        let max_size = peers.to_num_peers().max_evil() + 1;
        let mut code_vec: Vec<InviteCodePart> = peers
            .iter()
            .take(max_size)
            .map(|(peer, node_id)| InviteCodePart::Api {
                node_id: *node_id,
                peer: *peer,
            })
            .collect();
        code_vec.push(InviteCodePart::FederationId(federation_id));

        Self(code_vec)
    }

    /// Get all peer node ids in the [`InviteCode`].
    pub fn peers(&self) -> BTreeMap<PeerId, PublicKey> {
        self.0
            .iter()
            .filter_map(|entry| match entry {
                InviteCodePart::Api { node_id, peer } => Some((*peer, *node_id)),
                InviteCodePart::FederationId(_) => None,
            })
            .collect()
    }

    /// Returns the federation's ID that can be used to authenticate the config
    /// downloaded from the API.
    pub fn federation_id(&self) -> FederationId {
        self.0
            .iter()
            .find_map(|data| match data {
                InviteCodePart::FederationId(federation_id) => Some(*federation_id),
                InviteCodePart::Api { .. } => None,
            })
            .expect("Ensured by constructor")
    }
}

/// For extendability [`InviteCode`] consists of parts, where client can ignore
/// ones they don't understand.
///
/// Currently we always just use one `Api` and one `FederationId` variant in an
/// invite code, but more can be added in the future while still keeping the
/// invite code readable for older clients, which will just ignore the new
/// fields.
#[derive(Clone, Debug, Eq, PartialEq, Encodable, Decodable, Hash, Ord, PartialOrd)]
enum InviteCodePart {
    /// API endpoint of one of the guardians
    Api {
        /// Iroh public key of the peer's API endpoint
        node_id: PublicKey,
        /// Peer id of the host from the node id
        peer: PeerId,
    },

    /// Authentication id for the federation
    FederationId(FederationId),
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
        let string = String::deserialize(deserializer)?;
        picomint_base32::decode(&string).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use iroh_base::SecretKey;

    use super::*;

    #[test]
    fn test_invite_code_to_from_string() {
        let node_id = SecretKey::from_bytes(&[0x42; 32]).public();
        let parts = vec![
            InviteCodePart::Api {
                node_id,
                peer: PeerId::from(0),
            },
            InviteCodePart::FederationId(FederationId(
                bitcoin::hashes::sha256::Hash::from_str(
                    "bea7ff4116f2b1d324c7b5d699cce4ac7408cee41db2c88027e21b76fff3b9f4",
                )
                .expect("valid hash"),
            )),
        ];
        let invite_code = InviteCode(parts.clone());

        let encoded = picomint_base32::encode(&invite_code);
        let decoded: InviteCode = picomint_base32::decode(&encoded).expect("roundtrip parses");
        assert_eq!(decoded.0, parts);
    }
}
