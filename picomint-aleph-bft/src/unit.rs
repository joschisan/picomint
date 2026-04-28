use std::collections::BTreeMap;

use bitcoin::hashes::sha256;
use picomint_core::PeerId;
use picomint_encoding::{Decodable, Encodable};

/// Round number within a session. Round 0 is the first row of the DAG;
/// its units carry empty parent sets and are otherwise created and
/// disseminated like every other unit.
pub type Round = u16;

/// 32-byte digest identifying a unit; the consensus hash of its `Encodable`
/// form.
pub type UnitHash = sha256::Hash;

/// Bound bundle for unit payloads. `D` rides through `Unit`, `Graph`,
/// `Extender`, and `Message` purely as data — the protocol never inspects
/// it — but the wire encoding, cloning into the extender's store, and
/// task-spawned engines impose this combined surface. `Debug` is also
/// required so [`crate::Entry<D>`] can be a `redb::Value` (whose trait
/// requires `Debug`).
pub trait UnitData:
    std::fmt::Debug + Clone + Encodable + Decodable + Send + Sync + 'static
{
}

impl<T: std::fmt::Debug + Clone + Encodable + Decodable + Send + Sync + 'static> UnitData for T {}

/// One node in the consensus DAG.
///
/// A unit is uniquely identified by its `(session, round, creator)`
/// coordinate; at most one unit per coordinate can ever be confirmed.
/// `session` rides through the hash so that two units at the same
/// `(round, creator)` slot in distinct sessions are cryptographically
/// distinct — a stale Propose/Confirmed/Ack from session N arriving at a
/// peer in session N+1 fails to match the local slot and is discarded.
/// `parents` maps each parent's creator to its hash; for `round > 0` the
/// parent set must contain *exactly* `threshold` distinct creators all
/// at `round - 1`. Round-0 units carry an empty parent set. `data` is
/// the creator's payload at this slot, generic over the element type
/// `D`; once the total order is extracted, each unit's `data` items are
/// emitted in order keyed by the unit's creator.
#[derive(Debug, Clone, PartialEq, Eq, Encodable, Decodable)]
pub struct Unit<D: UnitData> {
    /// The session this unit belongs to. Part of the unit's identity so
    /// that stale traffic from a previous session cannot land in the
    /// current session's graph.
    pub session: u64,
    /// The round this unit belongs to.
    pub round: Round,
    /// `PeerId` of this unit's creator.
    pub creator: PeerId,
    /// Hashes of this unit's parents, keyed by their creator.
    pub parents: BTreeMap<PeerId, UnitHash>,
    /// Creator's payload for this slot.
    pub data: Vec<D>,
}

impl<D: UnitData> Unit<D> {
    /// SHA-256 of this unit's consensus encoding.
    pub fn hash(&self) -> UnitHash {
        self.consensus_hash_sha256()
    }
}
