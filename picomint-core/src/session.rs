use std::collections::BTreeMap;

use bitcoin::hashes::sha256;
use picomint_encoding::{Decodable, Encodable};

use crate::NodeId;
use crate::secp256k1::schnorr;
use crate::tx::ConsensusItem;

/// A consensus item accepted in the consensus
///
/// If two correct nodes obtain two ordered items from the broadcast they
/// are guaranteed to be in the same order. However, an ordered items is
/// only guaranteed to be seen by all correct nodes if a correct node decides to
/// accept it.
#[derive(Clone, Debug, PartialEq, Eq, Encodable, Decodable)]
pub struct AcceptedItem {
    pub node: NodeId,
    pub item: ConsensusItem,
}

/// A session as far as its accepted items go: how many, the header they
/// fold to and their wire size. Every node that folds the same items in
/// the same order holds the same state, whether it ordered them itself
/// or adopted them — the ordering loop keeps it running as items land, a
/// node resuming or adopting refolds its accepted items from disk, and a
/// node validating an outcome refolds the items it received.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionState {
    /// The position of the next accepted item, so also how many there are.
    pub index: u64,
    /// The digest the session is signed over: the hash of its index with
    /// every accepted item folded in, in order.
    pub header: sha256::Hash,
    /// The wire size of the accepted items, which is what cuts a session.
    pub bytes: usize,
}

impl SessionState {
    pub fn new(index: u32) -> Self {
        Self {
            index: 0,
            header: index.consensus_hash(),
            bytes: 0,
        }
    }

    /// Fold one more accepted item in.
    pub fn fold(&mut self, item: &AcceptedItem) {
        self.index += 1;
        self.header = (self.header, item).consensus_hash();
        self.bytes += item.consensus_encode_to_vec().len();
    }
}

/// The header of a session that accepted exactly these items.
pub fn session_header(session: u32, items: &[AcceptedItem]) -> sha256::Hash {
    items.iter().fold(session.consensus_hash(), |header, item| {
        (header, item).consensus_hash()
    })
}

/// The items ordered in a session and accepted by Picomint consensus —
/// empty if none were ordered or all were discarded — with the mint's
/// naive threshold secp schnorr signatures over their header. It lets a
/// recovering node verify the mint's consensus history.
#[derive(Clone, Debug, Encodable, Decodable, Eq, PartialEq)]
pub struct SessionOutcome {
    pub items: Vec<AcceptedItem>,
    pub signatures: BTreeMap<NodeId, schnorr::Signature>,
}
