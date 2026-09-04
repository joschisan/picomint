use bitcoin::hashes::sha256;
use picomint_encoding::{Decodable, Encodable};

use crate::tx::ConsensusItem;
use crate::{NodeId, secp256k1};

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

/// Items ordered in a single session that have been accepted by Picomint
/// consensus.
///
/// A running Mint produces a [`SessionOutcome`] every couple of minutes.
/// Therefore, just like in Bitcoin, a [`SessionOutcome`] might be empty if no
/// items are ordered in that time or all ordered items are discarded by
/// Picomint Consensus.
///
/// When session is closed it is signed over by the nodes and produces a
/// [`SignedSessionOutcome`].
#[derive(Clone, Debug, PartialEq, Eq, Encodable, Decodable)]
pub struct SessionOutcome {
    pub items: Vec<AcceptedItem>,
}

impl SessionOutcome {
    /// A block header pairs its index with the consensus hash of its
    /// [`AcceptedItem`]s. Headers are only ever generated for signing
    /// and verification — never persisted or sent — so the empty
    /// session needs no special case.
    pub fn header(&self, index: u32) -> (u32, sha256::Hash) {
        (index, self.items.consensus_hash())
    }
}

/// A [`SessionOutcome`], signed by the Mint.
///
/// A signed block combines a block with the naive threshold secp schnorr
/// signature for its header created by the mint. The signed blocks allow
/// clients and recovering nodes to verify the mints consensus
/// history. After a signed block has been created it is stored in the database.
#[derive(Clone, Debug, Encodable, Decodable, Eq, PartialEq)]
pub struct SignedSessionOutcome {
    pub session_outcome: SessionOutcome,
    pub signatures: std::collections::BTreeMap<NodeId, secp256k1::schnorr::Signature>,
}
