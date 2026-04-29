use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;

use derive_more::{Display, FromStr};
use serde::{Deserialize, Serialize};

use picomint_encoding::{Decodable, Encodable};
use picomint_redb::consensus_key;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    Encodable,
    Decodable,
    Display,
    FromStr,
)]
pub struct PeerId(u8);

consensus_key!(PeerId);

impl PeerId {
    pub fn to_usize(self) -> usize {
        self.0 as usize
    }
}

impl From<u8> for PeerId {
    fn from(id: u8) -> Self {
        Self(id)
    }
}

impl From<PeerId> for u8 {
    fn from(peer: PeerId) -> Self {
        peer.0
    }
}

/// Allowed federation sizes — every entry is `3f + 1` for some f ≥ 1.
/// `From<usize>` rejects anything outside this list.
pub const ALLOWED_FEDERATION_SIZES: &[usize] = &[4, 7, 10, 13, 16, 19];

/// The size of a federation, parameterized by `f` (the maximum tolerated
/// number of byzantine peers). picomint only supports federations of
/// size `3f + 1`, so storing `f` lets every derived quantity drop out
/// of one multiplication or addition with no rounding involved.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NumPeers(usize);

impl NumPeers {
    /// Returns an iterator over all peer IDs in the federation.
    pub fn peer_ids(self) -> impl Iterator<Item = PeerId> {
        (0u8..(self.total() as u8)).map(PeerId)
    }

    /// Total number of guardians: `3f + 1`.
    pub fn total(self) -> usize {
        3 * self.0 + 1
    }

    /// Maximum tolerated byzantine peers: `f`.
    pub fn max_evil(self) -> usize {
        self.0
    }

    /// Smallest set guaranteed to contain at least one honest peer: `f + 1`.
    pub fn one_honest(self) -> usize {
        self.0 + 1
    }

    /// Consensus / signature threshold: `2f + 1`.
    pub fn threshold(self) -> usize {
        2 * self.0 + 1
    }
}

impl From<usize> for NumPeers {
    fn from(total: usize) -> Self {
        assert!(
            ALLOWED_FEDERATION_SIZES.contains(&total),
            "federation size of {total} is not supported",
        );

        Self(total / 3)
    }
}

/// Types that can be easily converted to [`NumPeers`]
pub trait NumPeersExt {
    fn to_num_peers(&self) -> NumPeers;
}

impl<T> NumPeersExt for BTreeMap<PeerId, T> {
    fn to_num_peers(&self) -> NumPeers {
        NumPeers::from(self.len())
    }
}

impl NumPeersExt for BTreeSet<PeerId> {
    fn to_num_peers(&self) -> NumPeers {
        NumPeers::from(self.len())
    }
}
