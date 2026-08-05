use std::collections::BTreeMap;
use std::fmt::Debug;
use std::ops::RangeInclusive;

use bitcoin::hashes::Hash as _;
use bitcoin::hashes::sha256;
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

/// Hash identifying a unit body: the sha256 consensus-hash of the
/// encoded [`Unit`]. Parent references pin the exact parent body via
/// this hash, so a forked slot's branches are distinguishable — the
/// prerequisite for the fork-tolerant commit rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Encodable, Decodable)]
pub struct UnitHash(pub sha256::Hash);

picomint_redb::consensus_key!(UnitHash);

impl UnitHash {
    /// Smallest possible hash; range-scan lower bound.
    pub fn min() -> UnitHash {
        UnitHash(sha256::Hash::from_byte_array([0x00; 32]))
    }

    /// Largest possible hash; range-scan upper bound.
    pub fn max() -> UnitHash {
        UnitHash(sha256::Hash::from_byte_array([0xff; 32]))
    }
}

/// Full coordinate of a unit body: its `(round, creator)` position
/// plus the body hash that disambiguates fork branches within it.
/// Storage key of `BFT_UNITS` and element type of the `extended` /
/// `emitted` sets.
pub type Slot = (Round, PeerId, UnitHash);

/// Inclusive key range covering every slot of `round`.
pub(crate) fn round_range(round: Round) -> RangeInclusive<Slot> {
    (round, PeerId::from(0u8), UnitHash::min())..=(round, PeerId::from(u8::MAX), UnitHash::max())
}

/// One node in the consensus DAG, parameterized by the application
/// payload type `D`.
///
/// A unit is identified by the hash of its body ([`Unit::hash`]);
/// its `(round, creator)` coordinate is an annotation that names the
/// position the body claims. A Byzantine creator may sign several
/// bodies for one position — each is stored under its own hash-keyed
/// [`Slot`] and the commit rule elects at most one branch per
/// candidate. The session is *not* carried in the unit body —
/// instead, signatures are produced over the tuple `(session, unit)`,
/// so a stale unit from a previous session arriving at a peer in the
/// current session fails signature verification and is discarded.
/// This saves 8 bytes per unit on the wire vs. embedding the session
/// in the body.
///
/// `parents` maps each parent's creator to the hash of its body at
/// `round - 1`; for `round > 0` it must contain *exactly* `threshold`
/// entries. The map shape structurally enforces one parent per
/// creator. Round-0 units carry an empty parent map. `data` is the
/// creator's payload at this slot; once the total order is extracted,
/// each unit's `data` items are emitted in order keyed by the unit's
/// creator.
#[derive(Debug, Clone, PartialEq, Eq, Encodable, Decodable)]
pub struct Unit<D: UnitData> {
    /// The round this unit belongs to.
    pub round: Round,
    /// `PeerId` of this unit's creator.
    pub creator: PeerId,
    /// Creator and body hash of this unit's parents at `round - 1`.
    pub parents: BTreeMap<PeerId, UnitHash>,
    /// Creator's payload for this slot.
    pub data: Vec<D>,
}

picomint_redb::consensus_value!([D: UnitData] Unit<D>);

impl<D: UnitData> Unit<D> {
    /// The body hash identifying this unit; what parents reference and
    /// what keys the unit's row in `BFT_UNITS`.
    pub fn hash(&self) -> UnitHash {
        UnitHash(self.consensus_hash_sha256())
    }

    /// The unit's full storage coordinate: `(round, creator, hash)`.
    pub fn slot(&self) -> Slot {
        (self.round, self.creator, self.hash())
    }
}

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
