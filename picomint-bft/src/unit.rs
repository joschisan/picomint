use std::collections::BTreeSet;
use std::fmt::Debug;

use picomint_core::PeerId;
use picomint_core::secp256k1::schnorr;
use picomint_encoding::{Decodable, Encodable};

/// Round number within a session. Round 0 is the first row of the DAG;
/// its units carry empty parent sets and are otherwise created and
/// disseminated like every other unit.
pub type Round = u32;

/// Type alias for the trait bound every consumer of `D` ends up
/// repeating. Anything that round-trips on the wire, can be moved
/// across tasks, and lives as long as the program needs it to.
pub trait UnitData:
    Encodable + Decodable + Clone + Debug + PartialEq + Eq + Send + Sync + 'static
{
}

impl<T> UnitData for T where
    T: Encodable + Decodable + Clone + Debug + PartialEq + Eq + Send + Sync + 'static
{
}

/// One node in the consensus DAG, parameterized by the application
/// payload type `D`.
///
/// A unit is uniquely identified by its `(round, creator)` coordinate
/// within a session; peers store the first valid body they see per
/// slot. The session is *not* carried in the unit body — instead,
/// signatures are produced over the tuple `(session, unit)`, so a
/// stale unit from a previous session arriving at a peer in the
/// current session fails signature verification and is discarded.
/// This saves 8 bytes per unit on the wire vs. embedding the session
/// in the body.
///
/// `parents` is the set of parent creators; for `round > 0` it must
/// contain *exactly* `threshold` distinct creators, each referring to
/// the locally-stored unit at `(round - 1, creator)`. Round-0 units
/// carry an empty parent set. Parent hashes are not yet carried —
/// slot uniqueness currently rests on creators not equivocating; the
/// upcoming fork-tolerant commit rule will pin parents by hash.
/// `data` is the creator's payload at this slot; once the total order
/// is extracted, each unit's `data` items are emitted in order keyed
/// by the unit's creator.
#[derive(Debug, Clone, PartialEq, Eq, Encodable, Decodable)]
pub struct Unit<D: UnitData> {
    /// The round this unit belongs to.
    pub round: Round,
    /// `PeerId` of this unit's creator.
    pub creator: PeerId,
    /// Creators of this unit's parents at `round - 1`.
    pub parents: BTreeSet<PeerId>,
    /// Creator's payload for this slot.
    pub data: Vec<D>,
}

picomint_redb::consensus_value!([D: UnitData] Unit<D>);

/// A unit together with its creator's schnorr signature over
/// `(session, unit)`. The signature must live outside [`Unit`] — it
/// cannot cover itself — so this wrapper is the one shape units travel
/// on the wire and persist in storage.
#[derive(Debug, Clone, PartialEq, Eq, Encodable, Decodable)]
pub struct SignedUnit<D: UnitData> {
    /// The signed body.
    pub unit: Unit<D>,
    /// The creator's signature over `(session, unit)`.
    pub sig: schnorr::Signature,
}

picomint_redb::consensus_value!([D: UnitData] SignedUnit<D>);
