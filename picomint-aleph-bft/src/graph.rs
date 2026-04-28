use std::collections::BTreeMap;

use picomint_core::config::ALEPH_BFT_UNIT_BYTE_LIMIT;
use picomint_core::secp256k1::schnorr;
use picomint_core::{NumPeers, PeerId};
use picomint_encoding::{Decodable, Encodable};

use crate::keychain::Keychain;
use crate::unit::{Round, Unit, UnitData, UnitHash};

/// One slot in the DAG: the unit at `(round, creator)` plus the
/// co-signatures collected so far. Confirmation is derived from
/// `sigs.len() >= threshold` (or genesis, which is confirmed by fiat); the
/// hash is derived from the unit.
#[derive(Debug, Clone, Encodable, Decodable)]
pub struct Entry<D: UnitData> {
    unit: Unit<D>,
    sigs: BTreeMap<PeerId, schnorr::Signature>,
}

// `redb::Value` for `Entry<D>` over consensus encoding. Lives here
// rather than at the daemon's storage layer so callers can use
// `Entry<D>` directly as a typed redb value — the orphan rule blocks an
// `impl Value for Entry<D>` from being written downstream.
picomint_redb::consensus_value!([D: UnitData] Entry<D>);

impl<D: UnitData> Entry<D> {
    fn new(unit: Unit<D>) -> Self {
        Self {
            unit,
            sigs: BTreeMap::new(),
        }
    }

    /// The unit at this slot.
    pub fn unit(&self) -> &Unit<D> {
        &self.unit
    }

    /// Co-signatures collected at this slot, keyed by signer.
    pub fn sigs(&self) -> &BTreeMap<PeerId, schnorr::Signature> {
        &self.sigs
    }

    /// Whether this entry has crossed the confirmation threshold.
    pub fn is_confirmed(&self, threshold: usize) -> bool {
        self.sigs.len() >= threshold
    }

    /// SHA-256 of the unit's consensus encoding.
    pub fn hash(&self) -> UnitHash {
        self.unit.hash()
    }
}

/// Per-peer view of the consensus DAG.
///
/// The graph is keyed by `(round, creator)` because that's the load-bearing
/// uniqueness invariant of the protocol — at most one unit per slot can ever
/// confirm. `BTreeMap` ordering lets us iterate-and-count confirmed units at
/// a given round via `range`. The graph starts empty; round 0 units are
/// created and disseminated like every other round, except that they
/// carry empty parent sets.
///
/// The graph is session-scoped: every unit it holds carries the same
/// `session`, and `insert_unit` rejects any unit whose `session` doesn't
/// match. A stale unit from a different session can never enter the
/// graph and so cannot block the current session's `(round, creator)`
/// slot.
pub struct Graph<D: UnitData> {
    session: u64,
    n: NumPeers,
    units: BTreeMap<(Round, PeerId), Entry<D>>,
}

/// Outcome of attempting to insert a freshly-received unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertOutcome {
    /// Unit was added to the graph at its slot. Caller should sign it and
    /// gossip the ack.
    Accepted,
    /// Slot already held a different (or identical) unit; ignored. Honest
    /// peers won't co-sign a second unit at a slot they've already endorsed.
    Duplicate,
    /// One or more parents are not yet in our graph, or are present but not
    /// confirmed; reject. The creator's perpetual rebroadcast will deliver
    /// it again once we've caught up.
    MissingParents,
    /// Parent set malformed (wrong size, hash mismatch with the slot we
    /// hold).
    InvalidParents,
    /// Unit's `session` doesn't match the graph's session. Likely a
    /// stale message from a previous session that's still in flight on
    /// the shared transport.
    WrongSession,
    /// Unit's `data` payload, re-encoded to bytes, exceeds
    /// [`ALEPH_BFT_UNIT_BYTE_LIMIT`]. Caps each peer's per-unit RAM
    /// footprint regardless of how many items the creator stuffed in.
    OversizedData,
}

/// Outcome of recording a co-signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SigOutcome {
    /// Signature recorded but threshold not yet reached.
    Recorded,
    /// Signature recorded and the unit just crossed the confirmation
    /// threshold. Caller should check whether round-N+1 creation is now
    /// triggerable.
    Confirmed,
    /// We don't have a unit at `(round, creator)` yet, or the signature
    /// doesn't verify against the unit we hold there, or we already had
    /// this signer's sig, or the slot is already confirmed.
    Discarded,
}

impl<D: UnitData> Graph<D> {
    /// Create an empty graph for `session`. Round-0 units are created
    /// and disseminated like every other round.
    pub fn new(n: NumPeers, session: u64) -> Self {
        Self {
            session,
            n,
            units: BTreeMap::new(),
        }
    }

    /// The session this graph is scoped to. Every unit it holds carries
    /// this `session`.
    pub fn session(&self) -> u64 {
        self.session
    }

    /// Number of co-signatures required for a non-genesis unit to confirm.
    pub fn threshold(&self) -> usize {
        self.n.threshold()
    }

    /// Iterate every peer id in the federation in `PeerId` order.
    pub fn peer_ids(&self) -> impl Iterator<Item = PeerId> {
        self.n.peer_ids()
    }

    /// Get the entry at `(round, creator)`, if any.
    pub fn entry(&self, round: Round, creator: PeerId) -> Option<&Entry<D>> {
        self.units.get(&(round, creator))
    }

    /// Whether the slot at `(round, creator)` exists and has crossed the
    /// confirmation threshold (or is a genesis slot).
    pub fn is_confirmed(&self, round: Round, creator: PeerId) -> bool {
        self.units
            .get(&(round, creator))
            .is_some_and(|e| e.is_confirmed(self.threshold()))
    }

    /// Iterate the slots at `round` in `creator`-order.
    pub fn round_units(&self, round: Round) -> impl Iterator<Item = &Entry<D>> {
        self.units
            .range((round, PeerId::from(0u8))..)
            .take_while(move |((r, _), _)| *r == round)
            .map(|(_, e)| e)
    }

    /// Number of confirmed units at `round`.
    pub fn confirmed_count(&self, round: Round) -> usize {
        self.round_units(round)
            .filter(|e| e.is_confirmed(self.threshold()))
            .count()
    }

    /// Place a previously-persisted entry back into the graph at its slot.
    ///
    /// Used at startup to restore state from [`crate::Backup`]. Skips the
    /// parent-confirmation check that `insert_unit` enforces — we trust
    /// what we wrote ourselves, and replaying the saved
    /// `(round, peer)`-keyed entries in lex order means parents are
    /// always restored before their children. Caller must filter the
    /// backup stream by `session()` before calling.
    pub fn restore_entry(&mut self, entry: Entry<D>) {
        assert_eq!(
            entry.unit.session, self.session,
            "restore_entry called with a unit from a different session",
        );

        self.units
            .insert((entry.unit.round, entry.unit.creator), entry);
    }

    /// Insert a freshly-received unit into the graph.
    ///
    /// First-seen wins per slot — if we already hold any unit at
    /// `(unit.round, unit.creator)`, the second is rejected as `Duplicate`,
    /// even if the bytes differ. This is what makes consistent broadcast
    /// safe: an honest peer never co-signs two distinct units at the same
    /// slot, so a forker can't reach threshold on either side.
    ///
    /// All parents must already be in our graph and confirmed; the parent
    /// hashes claimed by the unit must match the hashes we hold at those
    /// slots. Otherwise we reject; the creator's rebroadcast will retry.
    pub fn insert_unit(&mut self, unit: Unit<D>) -> InsertOutcome {
        if unit.session != self.session {
            return InsertOutcome::WrongSession;
        }

        if self.units.contains_key(&(unit.round, unit.creator)) {
            return InsertOutcome::Duplicate;
        }

        // Re-encode the payload and reject anything past the byte cap.
        // `D` is generic, so the bound has to be checked here rather
        // than at decode time — a malicious creator that bundles too
        // many items into one unit gets dropped before we keep the
        // entry around.
        if unit.data.consensus_encode_to_vec().len() > ALEPH_BFT_UNIT_BYTE_LIMIT {
            return InsertOutcome::OversizedData;
        }

        match self.validate_parents(&unit) {
            ParentCheck::Ok => {}
            ParentCheck::Missing => return InsertOutcome::MissingParents,
            ParentCheck::Invalid => return InsertOutcome::InvalidParents,
        }

        let entry = Entry::new(unit);

        self.units
            .insert((entry.unit.round, entry.unit.creator), entry);

        InsertOutcome::Accepted
    }

    /// Record a co-signature on the unit at `(round, creator)`. The signature
    /// is verified against the hash of the unit *we currently hold* at that
    /// slot — this is the consistent-broadcast safety check, since a forker
    /// trying to split co-signers across two distinct units will find that
    /// each peer's collected sigs only verify against their local unit.
    pub fn record_sig(
        &mut self,
        round: Round,
        creator: PeerId,
        signer: PeerId,
        signature: schnorr::Signature,
        keychain: &Keychain,
    ) -> SigOutcome {
        let threshold = self.threshold();

        let Some(entry) = self.units.get_mut(&(round, creator)) else {
            return SigOutcome::Discarded;
        };

        if entry.is_confirmed(threshold) {
            return SigOutcome::Discarded;
        }

        if entry.sigs.contains_key(&signer) {
            return SigOutcome::Discarded;
        }

        if !keychain.verify(&entry.hash(), &signature, signer) {
            return SigOutcome::Discarded;
        }

        entry.sigs.insert(signer, signature);

        if entry.sigs.len() >= threshold {
            return SigOutcome::Confirmed;
        }

        SigOutcome::Recorded
    }

    /// Build a candidate parent set for a `(round, creator)` unit.
    ///
    /// For `round == 0`, returns an empty parent set unconditionally —
    /// round-0 units are the DAG's roots.
    ///
    /// For `round > 0`, returns `creator`'s own confirmed slot at
    /// `round - 1` plus the lowest-PeerId-keyed confirmed others up to
    /// `threshold`, or `None` if either `creator`'s own previous-round
    /// slot is missing/unconfirmed or fewer than `threshold` slots at
    /// `round - 1` are confirmed.
    ///
    /// The exact-threshold rule means we always pick exactly `threshold`
    /// parents, never more. The chain rule means `creator`'s own previous-
    /// round unit is always among them, so each peer's units form an
    /// unbroken chain across rounds.
    pub fn parents_for(&self, round: Round, creator: PeerId) -> Option<BTreeMap<PeerId, UnitHash>> {
        if round == 0 {
            return Some(BTreeMap::new());
        }

        let parent_round = round - 1;
        let threshold = self.threshold();

        let creator_entry = self.entry(parent_round, creator)?;

        if !creator_entry.is_confirmed(threshold) {
            return None;
        }

        let mut parents = BTreeMap::new();
        parents.insert(creator, creator_entry.hash());

        for entry in self.round_units(parent_round) {
            if parents.len() == threshold {
                break;
            }
            if entry.unit.creator == creator {
                continue;
            }
            if !entry.is_confirmed(threshold) {
                continue;
            }
            parents.insert(entry.unit.creator, entry.hash());
        }

        (parents.len() == threshold).then_some(parents)
    }

    fn validate_parents(&self, unit: &Unit<D>) -> ParentCheck {
        let t = self.threshold();

        if unit.round == 0 {
            return match unit.parents.is_empty() {
                true => ParentCheck::Ok,
                false => ParentCheck::Invalid,
            };
        }

        if unit.parents.len() != t {
            return ParentCheck::Invalid;
        }

        if !unit.parents.contains_key(&unit.creator) {
            return ParentCheck::Invalid;
        }

        for (p_creator, p_hash) in &unit.parents {
            let Some(p_entry) = self.units.get(&(unit.round - 1, *p_creator)) else {
                return ParentCheck::Missing;
            };

            if !p_entry.is_confirmed(t) {
                return ParentCheck::Missing;
            }

            if p_entry.hash() != *p_hash {
                return ParentCheck::Invalid;
            }
        }

        ParentCheck::Ok
    }
}

enum ParentCheck {
    Ok,
    Missing,
    Invalid,
}
