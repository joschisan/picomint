use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;

use derive_more::{Display, FromStr};
use serde::{Deserialize, Serialize};

use picomint_encoding::{Decodable, Encodable};

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
pub struct NodeId(u8);

impl NodeId {
    pub fn to_usize(self) -> usize {
        self.0 as usize
    }

    pub fn to_u64(self) -> u64 {
        self.0 as u64
    }
}

impl From<u8> for NodeId {
    fn from(id: u8) -> Self {
        Self(id)
    }
}

impl From<NodeId> for u8 {
    fn from(node: NodeId) -> Self {
        node.0
    }
}

/// Allowed mint sizes — every entry is `3f + 1` for some f ≥ 1.
/// `From<usize>` rejects anything outside this list.
pub const ALLOWED_MINT_SIZES: &[usize] = &[4, 7, 10, 13, 16, 19, 22];

/// The size of a mint, parameterized by `f` (the maximum tolerated
/// number of byzantine nodes). picomint only supports mints of
/// size `3f + 1`, so storing `f` lets every derived quantity drop out
/// of one multiplication or addition with no rounding involved.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NumPeers(usize);

impl NumPeers {
    /// Returns an iterator over all node IDs in the mint.
    pub fn node_ids(self) -> impl Iterator<Item = NodeId> {
        (0u8..(self.total() as u8)).map(NodeId)
    }

    /// Total number of guardians: `3f + 1`.
    pub fn total(self) -> usize {
        3 * self.0 + 1
    }

    /// Maximum tolerated byzantine nodes: `f`.
    pub fn max_evil(self) -> usize {
        self.0
    }

    /// Smallest set guaranteed to contain at least one honest node: `f + 1`.
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
            ALLOWED_MINT_SIZES.contains(&total),
            "mint size of {total} is not supported",
        );

        Self(total / 3)
    }
}

/// Types that can be easily converted to [`NumPeers`]
pub trait NumPeersExt {
    fn to_num_peers(&self) -> NumPeers;
}

impl<T> NumPeersExt for BTreeMap<NodeId, T> {
    fn to_num_peers(&self) -> NumPeers {
        NumPeers::from(self.len())
    }
}

impl NumPeersExt for BTreeSet<NodeId> {
    fn to_num_peers(&self) -> NumPeers {
        NumPeers::from(self.len())
    }
}
