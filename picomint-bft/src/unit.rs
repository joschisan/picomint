use std::collections::BTreeSet;
use std::fmt::Debug;

use picomint_core::PeerId;
use picomint_encoding::{Decodable, Encodable};

/// Round number within a session. Round 0 is the first row of the DAG;
/// its units carry empty parent sets and are otherwise created and
/// disseminated like every other unit.
pub type Round = u16;

/// Bound bundle for unit payloads. `D` rides through `Unit`, `Graph`,
/// `Extender`, and `Message` purely as data — the protocol never inspects
/// it — but the wire encoding, cloning into the extender's store, and
/// task-spawned engines impose this combined surface. `Debug` is also
/// required so [`crate::Entry<D>`] can be a `redb::Value` (whose trait
/// requires `Debug`).
pub trait UnitData: Debug + Clone + Encodable + Decodable + Send + Sync + 'static {}

impl<T: Debug + Clone + Encodable + Decodable + Send + Sync + 'static> UnitData for T {}

/// One node in the consensus DAG.
///
/// A unit is uniquely identified by its `(session, round, creator)`
/// coordinate; at most one unit per coordinate can ever be confirmed.
/// `session` rides through the hash so that two units at the same
/// `(round, creator)` slot in distinct sessions are cryptographically
/// distinct — a stale Propose/Confirmed/Ack from session N arriving at a
/// peer in session N+1 fails to match the local slot and is discarded.
/// `parents` is the set of parent creators; for `round > 0` it must
/// contain *exactly* `threshold` distinct creators, each referring to
/// the (unique, locally-confirmed) unit at `(round - 1, creator)`.
/// Round-0 units carry an empty parent set. Parent hashes are not
/// carried — at most one unit per slot can ever confirm, so the creator
/// is sufficient to identify the parent. `data` is the creator's
/// payload at this slot, generic over the element type `D`; once the
/// total order is extracted, each unit's `data` items are emitted in
/// order keyed by the unit's creator.
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
    /// Creators of this unit's parents at `round - 1`.
    pub parents: BTreeSet<PeerId>,
    /// Creator's payload for this slot.
    pub data: Vec<D>,
}
