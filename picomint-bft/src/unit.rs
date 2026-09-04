use std::collections::BTreeMap;
use std::fmt::Debug;

use bitcoin::hashes::sha256;
use picomint_core::NodeId;
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

/// Hash identifying a unit: the sha256 consensus-hash of the encoded
/// [`Unit`]. The unit's identity everywhere — storage key of
/// `BFT_UNITS`, element of the in-memory `extended` / `emitted` sets,
/// and how parents pin the exact parent unit, so a forked position's
/// branches are distinguishable — the prerequisite for the
/// fork-tolerant commit rule. Covers the payload transitively through
/// [`Unit::data`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Encodable, Decodable)]
pub struct UnitHash(pub sha256::Hash);

/// One node in the consensus DAG: position, parent map, and payload
/// commitment — everything the ordering engine tallies over, and the
/// exact record the in-memory `extended` map holds. The payload
/// itself rides next to it in [`UnitEnvelope`].
///
/// A unit is identified by the hash of its encoding ([`Unit::hash`]);
/// its `(round, creator)` coordinate is an annotation that names the
/// position the unit claims. A Byzantine creator may sign several
/// units for one position — each is stored under its own hash and the
/// commit rule elects at most one branch per candidate. The session
/// is *not* carried in the unit — instead, signatures are produced
/// over the tuple `(session, unit)`, so a stale unit from a previous
/// session arriving at a node in the current session fails signature
/// verification and is discarded. This saves 4 bytes per unit on the
/// wire vs. embedding the session in the unit.
///
/// `parents` maps each parent's creator to the hash of its unit at
/// `round - 1`; for `round > 0` it must contain *exactly* `threshold`
/// entries. The map shape structurally enforces one parent per
/// creator. Round-0 units carry an empty parent map.
#[derive(Debug, Clone, PartialEq, Eq, Encodable, Decodable)]
pub struct Unit {
    /// The round this unit belongs to.
    pub round: Round,
    /// `NodeId` of this unit's creator.
    pub creator: NodeId,
    /// Creator and unit hash of this unit's parents at `round - 1`.
    pub parents: BTreeMap<NodeId, UnitHash>,
    /// The sha256 consensus-hash of the payload carried in this
    /// unit's envelope; `None` iff the payload is empty.
    pub data: Option<sha256::Hash>,
}

impl Unit {
    /// The hash identifying this unit; what parents reference and
    /// what keys the unit's row in `BFT_UNITS`.
    pub fn hash(&self) -> UnitHash {
        UnitHash(self.consensus_hash_sha256())
    }
}

/// The wire and storage envelope: the identity-bearing unit, the
/// payload its `data` commitment pins, and the creator's schnorr
/// signature over `(session, unit)`. The signature must live outside
/// [`Unit`] — it cannot cover itself — and the payload is freight the
/// ordering engine only reads at emission, so both ride alongside.
#[derive(Debug, Clone, PartialEq, Eq, Encodable, Decodable)]
pub struct UnitEnvelope<D: UnitData> {
    /// The signed unit.
    pub unit: Unit,
    /// The creator's payload; once the total order is extracted, each
    /// unit's items are emitted in order keyed by the unit's creator.
    pub data: Vec<D>,
    /// The creator's signature over `(session, unit)`.
    pub sig: schnorr::Signature,
}
