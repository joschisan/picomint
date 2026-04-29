use std::collections::BTreeMap;

use async_channel::Sender;
use picomint_core::config::BFT_UNIT_BYTE_LIMIT;
use picomint_core::secp256k1::schnorr;
use picomint_core::{NumPeers, PeerId};
use picomint_encoding::{Decodable, Encodable};

use crate::backup::DynBackup;
use crate::extender::Extender;
use crate::keychain::Keychain;
use crate::unit::{Round, Unit, UnitData, UnitHash};

/// One slot in the DAG: the unit at `(round, creator)` and the
/// co-signatures collected so far.
///
/// `insert_unit` is strict on parents — every parent must already be
/// confirmed locally at insert time — so confirmation reduces to
/// `sigs.len() >= threshold` and there's nothing to cache.
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

    /// Whether this entry has crossed the co-signature threshold.
    /// Monotonic: once true, stays true. Strict insert guarantees the
    /// parents were confirmed before this entry entered the graph, so
    /// no per-parent re-check is needed.
    pub fn is_confirmed(&self, threshold: usize) -> bool {
        self.sigs.len() >= threshold
    }

    /// SHA-256 of the unit's consensus encoding.
    pub fn hash(&self) -> UnitHash {
        self.unit.hash()
    }
}

/// Per-peer view of the consensus DAG plus the persistence and
/// ordering machinery downstream of it.
///
/// The DAG is keyed by `(round, creator)` because that's the
/// load-bearing uniqueness invariant of the protocol — at most one
/// unit per slot can ever confirm. `BTreeMap` ordering lets us
/// iterate-and-count confirmed units at a given round via `range`.
/// Round-0 units are created and disseminated like every other round
/// except that they carry empty parent sets.
///
/// The graph also owns its `Backup` and `Extender`: every mutation
/// that changes an entry persists it through `backup`, and every
/// confirmation transition feeds the unit through `extender` so its
/// causal closure can be ordered. Engine code never touches backup or
/// extender directly.
///
/// Session-scoped: every unit holds the same `session`, and
/// `insert_unit` rejects mismatches. A stale unit from a previous
/// session can't enter the graph and so cannot block the current
/// session's `(round, creator)` slot.
pub struct Graph<D: UnitData> {
    session: u64,
    n: NumPeers,
    units: BTreeMap<(Round, PeerId), Entry<D>>,
    backup: DynBackup<D>,
    extender: Extender<D>,
}

/// Outcome of attempting to insert a freshly-received unit.
///
/// On `Accepted` the entry has been added to the graph and persisted to
/// backup as a side effect; the caller doesn't need to follow up. Insert
/// is *strict* on parents — every parent must already be confirmed
/// locally; otherwise the unit is rejected with `MissingParents` and the
/// caller drops it (a later anti-entropy cycle will re-deliver it once
/// the deficit is repaired).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome {
    /// Unit was added to the graph at its slot and saved to backup.
    /// Caller should sign it and gossip the ack so peers union our
    /// contribution.
    Accepted,
    /// Slot already held a different (or identical) unit; ignored. Honest
    /// peers won't co-sign a second unit at a slot they've already endorsed.
    Duplicate,
    /// At least one parent slot is either absent from our graph or
    /// present-but-unconfirmed. Drop the unit; the periodic anti-entropy
    /// (broadcast + per-creator pull) will refill the missing prerequisites
    /// and the unit will be re-delivered on a later cycle.
    MissingParents,
    /// Parent set malformed: wrong size for the round, or a parent hash
    /// doesn't match the entry we already hold at that slot.
    InvalidParents,
    /// Unit's `session` doesn't match the graph's session. Likely a
    /// stale message from a previous session that's still in flight on
    /// the shared transport.
    WrongSession,
    /// Unit's `data` payload, re-encoded to bytes, exceeds
    /// [`BFT_UNIT_BYTE_LIMIT`]. Caps each peer's per-unit RAM
    /// footprint regardless of how many items the creator stuffed in.
    OversizedData,
}

/// Outcome of recording a co-signature.
///
/// On `Accepted` the sig was added to the slot, the entry has been
/// re-saved to backup, and — if the sig pushed the slot across the
/// threshold — the unit has been fed to the extender. The caller
/// doesn't need to follow up. Strict insert means every parent of an
/// in-graph entry is already confirmed, so a sig flip is purely local
/// — there is no cascade up to higher-round slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigOutcome {
    /// Signature was added to the slot.
    Accepted,
    /// We don't have a unit at `(round, creator)` yet, or the signature
    /// doesn't verify against the unit we hold there, or we already had
    /// this signer's sig, or the slot is already confirmed.
    Discarded,
}

impl<D: UnitData> Graph<D> {
    /// Build a graph for `session`, restoring any persisted state from
    /// `backup` and feeding the recovered confirmed units into a fresh
    /// internal extender that emits ordered datums on `ordered_tx`.
    /// Round-0 units are created and disseminated like every other round.
    pub fn new(
        n: NumPeers,
        session: u64,
        backup: DynBackup<D>,
        ordered_tx: Sender<(Round, PeerId, D)>,
    ) -> Self {
        // Restore persisted entries in (round, peer) lex order — same
        // order the BTreeMap would iterate them after insert, so parents
        // restore before children. Confirmed entries are also fed into
        // the extender so post-restart ordering resumes from the right
        // place.
        let mut extender = Extender::new(n, ordered_tx);
        let mut units = BTreeMap::new();

        for entry in backup.load() {
            assert_eq!(
                entry.unit.session, session,
                "backup session does not match graph session",
            );
            if entry.is_confirmed(n.threshold()) {
                extender.add_unit(entry.unit.clone());
            }
            units.insert((entry.unit.round, entry.unit.creator), entry);
        }

        Self {
            session,
            n,
            units,
            backup,
            extender,
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

    /// Total number of peers in the federation.
    pub fn num_peers(&self) -> NumPeers {
        self.n
    }

    /// Iterate every peer id in the federation in `PeerId` order.
    pub fn peer_ids(&self) -> impl Iterator<Item = PeerId> {
        self.n.peer_ids()
    }

    /// Get the entry at `(round, creator)`, if any.
    pub fn entry(&self, round: Round, creator: PeerId) -> Option<&Entry<D>> {
        self.units.get(&(round, creator))
    }

    /// Whether the slot at `(round, creator)` exists and is confirmed
    /// (sigs threshold met). Strict insert guarantees parents were
    /// confirmed when the entry entered the graph.
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
        let t = self.threshold();
        self.round_units(round)
            .filter(|e| e.is_confirmed(t))
            .count()
    }

    /// Highest-round entry we hold for `creator`, if any. Used by the
    /// periodic anti-entropy push: each peer sends its highest known
    /// unit per other peer (with the sigs it holds) to everyone, which
    /// both refills sig deficits at slots receivers already hold and
    /// seeds higher rounds at laggards.
    pub fn highest_entry(&self, creator: PeerId) -> Option<&Entry<D>> {
        self.units
            .iter()
            .rev()
            .find_map(|((_, c), e)| (*c == creator).then_some(e))
    }

    /// Lowest round where `(round, creator)` is either absent from our
    /// graph or present-but-unconfirmed. Used by the periodic anti-entropy
    /// pull: for each peer in the federation we issue one
    /// `Message::Request { round, creator }` per cycle to refill the
    /// next gap along that peer's column. Idempotent and drop-tolerant —
    /// the same round is requested again on the next cycle until it
    /// confirms.
    pub fn lowest_unconfirmed_round(&self, creator: PeerId) -> Round {
        let t = self.threshold();
        let mut round: Round = 0;
        while let Some(entry) = self.units.get(&(round, creator)) {
            if !entry.is_confirmed(t) {
                return round;
            }
            round += 1;
        }
        round
    }

    /// Insert a freshly-received unit into the graph.
    ///
    /// First-seen wins per slot — if we already hold any unit at
    /// `(unit.round, unit.creator)`, the second is rejected as `Duplicate`,
    /// even if the bytes differ. This is what makes consistent broadcast
    /// safe: an honest peer never co-signs two distinct units at the same
    /// slot, so a forker can't reach threshold on either side.
    ///
    /// Insert is *strict* on parents: every parent must already be present
    /// and confirmed locally. If any parent is missing or unconfirmed the
    /// unit is rejected with `MissingParents` and the caller drops it. The
    /// periodic anti-entropy will refill the prerequisites and the unit
    /// will be re-delivered (via the periodic broadcast) on a later cycle.
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
        if unit.data.consensus_encode_to_vec().len() > BFT_UNIT_BYTE_LIMIT {
            return InsertOutcome::OversizedData;
        }

        if let Some(reason) = self.check_parents(&unit) {
            return reason;
        }

        let entry = Entry::new(unit);

        self.backup.save(&entry);

        self.units
            .insert((entry.unit.round, entry.unit.creator), entry);

        InsertOutcome::Accepted
    }

    /// Record a co-signature on the unit at `(round, creator)`. The signature
    /// is verified against the hash of the unit *we currently hold* at that
    /// slot — this is the consistent-broadcast safety check, since a forker
    /// trying to split co-signers across two distinct units will find that
    /// each peer's collected sigs only verify against their local unit.
    ///
    /// Strict insert guarantees the slot's parents are already confirmed,
    /// so when this sig brings the count to threshold the slot flips
    /// directly with no parent re-check and no cascade upward.
    pub fn record_sig(
        &mut self,
        round: Round,
        creator: PeerId,
        signer: PeerId,
        signature: schnorr::Signature,
        keychain: &Keychain,
    ) -> SigOutcome {
        let t = self.threshold();

        let Some(entry) = self.units.get_mut(&(round, creator)) else {
            return SigOutcome::Discarded;
        };

        if entry.sigs.len() >= t {
            return SigOutcome::Discarded;
        }

        if entry.sigs.contains_key(&signer) {
            return SigOutcome::Discarded;
        }

        if !keychain.verify(&entry.unit, &signature, signer) {
            return SigOutcome::Discarded;
        }

        entry.sigs.insert(signer, signature);

        self.backup.save(entry);

        if entry.sigs.len() == t {
            // This sig pushed the slot across the threshold. Hand the
            // unit to the extender — strict insert means every parent
            // is already confirmed, so this is the only confirmation
            // event for the slot and there's no cascade upward.
            self.extender.add_unit(entry.unit.clone());
        }

        SigOutcome::Accepted
    }

    /// Build a candidate parent set for a unit at `round`.
    ///
    /// For `round == 0`, returns an empty parent set unconditionally —
    /// round-0 units are the DAG's roots.
    ///
    /// For `round > 0`, returns the lowest-`PeerId`-keyed `threshold`
    /// confirmed slots at `round - 1`, or `None` if fewer than
    /// `threshold` slots at that round are confirmed.
    ///
    /// No chain rule: a creator's own previous-round unit is *not*
    /// forced into the parent set. Recovery is independent of the
    /// chain — the periodic anti-entropy push (`broadcast_peer_units`)
    /// refills sig deficits at slots receivers already hold, and the
    /// per-creator pull (`Message::Request` for the lowest unconfirmed
    /// round) pulls in missing units one round at a time.
    pub fn parents_for(&self, round: Round) -> Option<BTreeMap<PeerId, UnitHash>> {
        let Some(parent_round) = round.checked_sub(1) else {
            return Some(BTreeMap::new());
        };

        let t = self.threshold();

        let parents: BTreeMap<PeerId, UnitHash> = self
            .round_units(parent_round)
            .filter(|e| e.is_confirmed(t))
            .take(t)
            .map(|e| (e.unit.creator, e.hash()))
            .collect();

        (parents.len() == t).then_some(parents)
    }

    /// Strict parent check used by `insert_unit`. Returns `Some(reason)`
    /// when the parent set is unacceptable, `None` when it's good to go.
    ///
    /// - Round 0 must have an empty parent set.
    /// - Round R>0 must carry exactly `threshold` parents.
    /// - Every parent slot must already be in our graph and confirmed.
    /// - Every parent's stored hash must match the claim.
    fn check_parents(&self, unit: &Unit<D>) -> Option<InsertOutcome> {
        let t = self.threshold();

        if unit.round == 0 {
            return (!unit.parents.is_empty()).then_some(InsertOutcome::InvalidParents);
        }

        if unit.parents.len() != t {
            return Some(InsertOutcome::InvalidParents);
        }

        for (p_creator, p_hash) in &unit.parents {
            let Some(parent) = self.units.get(&(unit.round - 1, *p_creator)) else {
                return Some(InsertOutcome::MissingParents);
            };

            if parent.hash() != *p_hash {
                return Some(InsertOutcome::InvalidParents);
            }

            if !parent.is_confirmed(t) {
                return Some(InsertOutcome::MissingParents);
            }
        }

        None
    }
}
