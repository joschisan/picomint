use std::collections::{BTreeMap, BTreeSet};

use async_channel::Sender;
use picomint_core::config::BFT_UNIT_BYTE_LIMIT;
use picomint_core::secp256k1::schnorr;
use picomint_core::{NumPeers, PeerId};
use picomint_encoding::{Decodable, Encodable};

use crate::backup::DynBackup;
use crate::extender::Extender;
use crate::keychain::Keychain;
use crate::unit::{Round, Unit, UnitData};

/// One slot in the DAG: the unit at `(round, creator)`, the creator's
/// signature over its body, and the co-signatures collected so far.
///
/// The creator's sig lives in its own field — it's structurally
/// distinct from cosigs (it's what binds the body to its claimed author
/// and is part of the on-the-wire `Unit` message), and separating the
/// two avoids a "is this entry in the map yet" check on every cosig
/// recording.
///
/// Insert is *lax* on parents — a unit's body and accumulated sigs land
/// in the graph regardless of whether its ancestors are present yet.
/// "Confirmed" therefore only means `1 + cosigs.len() >= threshold`
/// (creator's sig plus enough cosigs); whether the slot's full ancestor
/// closure is also locally ready is tracked separately on `Graph` and
/// gates promotion into the extender (and own-unit construction).
#[derive(Debug, Clone, Encodable, Decodable)]
pub struct Entry<D: UnitData> {
    unit: Unit<D>,
    sig: schnorr::Signature,
    cosigs: BTreeMap<PeerId, schnorr::Signature>,
}

// `redb::Value` for `Entry<D>` over consensus encoding. Lives here
// rather than at the daemon's storage layer so callers can use
// `Entry<D>` directly as a typed redb value — the orphan rule blocks an
// `impl Value for Entry<D>` from being written downstream.
picomint_redb::consensus_value!([D: UnitData] Entry<D>);

impl<D: UnitData> Entry<D> {
    fn new(
        unit: Unit<D>,
        sig: schnorr::Signature,
        cosigs: BTreeMap<PeerId, schnorr::Signature>,
    ) -> Self {
        Self { unit, sig, cosigs }
    }

    /// The unit at this slot.
    pub fn unit(&self) -> &Unit<D> {
        &self.unit
    }

    /// The creator's schnorr signature over the unit's consensus
    /// encoding. Always present — every entry that exists carries it.
    pub fn sig(&self) -> &schnorr::Signature {
        &self.sig
    }

    /// Co-signatures collected from non-creator peers, keyed by signer.
    pub fn cosigs(&self) -> &BTreeMap<PeerId, schnorr::Signature> {
        &self.cosigs
    }

    /// Whether this entry has crossed the threshold of total signers
    /// (creator + cosigners). Monotonic: once true, stays true.
    /// Confirmation here is the purely-local "enough sigs" predicate —
    /// it does *not* imply the slot's ancestors are present and ready.
    /// Use [`Graph::is_fed`] for the stronger ancestrally-ready
    /// predicate.
    pub fn is_confirmed(&self, threshold: usize) -> bool {
        1 + self.cosigs.len() >= threshold
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
/// Insert is split into two gates:
///
/// - **Graph admission** (`insert_unit`) is lax. A well-formed signed
///   unit lands in `units` regardless of whether its ancestors are
///   present yet, so out-of-order delivery accumulates sigs and
///   gossips immediately rather than being dropped and refetched.
/// - **Promotion** (`fed`) is strict. A slot is promoted into the
///   extender — and made eligible as a parent for our own future
///   units via [`Self::parents_for`] — only once it has crossed the
///   sig threshold *and* every parent slot is itself promoted. Promotion
///   cascades: when a slot becomes promotable, descendants whose last
///   missing parent was this slot are checked too.
///
/// The graph also owns its `Backup` and `Extender`: every mutation
/// that changes an entry persists it through `backup`, and every
/// promotion transition feeds the unit through `extender` so its
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
    /// Slots that have been promoted into the extender. Inductive
    /// invariant: a slot is in `fed` iff it's confirmed locally and
    /// every parent slot is also in `fed`. So membership in `fed` is
    /// also the predicate gating own-unit parent selection — we only
    /// build new units atop slots whose full ancestry we hold ready.
    fed: BTreeSet<(Round, PeerId)>,
    backup: DynBackup<D>,
    extender: Extender<D>,
}

impl<D: UnitData> Graph<D> {
    /// Build a graph for `session`, restoring any persisted state from
    /// `backup` and feeding the recovered fully-ancestored confirmed
    /// units into a fresh internal extender that emits ordered datums
    /// on `ordered_tx`. Round-0 units are created and disseminated like
    /// every other round.
    pub fn new(
        n: NumPeers,
        session: u64,
        backup: DynBackup<D>,
        ordered_tx: Sender<(Round, PeerId, D)>,
    ) -> Self {
        let extender = Extender::new(n, ordered_tx);

        let mut graph = Self {
            session,
            n,
            units: BTreeMap::new(),
            fed: BTreeSet::new(),
            backup,
            extender,
        };

        // `Backup::load` returns entries in `(round, peer)` lex order
        // — a valid topological order over the parent relation — so
        // inserting and immediately promoting in the same pass works:
        // by the time we promote `(R, c)`, every `(R-1, *)` slot that
        // could be a parent has already been processed and is either
        // in `fed` or never will be.
        for entry in graph.backup.load() {
            let r = entry.unit.round;
            let c = entry.unit.creator;
            graph.units.insert((r, c), entry);
            graph.try_promote(r, c);
        }

        graph
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

    /// Whether the slot at `(round, creator)` exists and is confirmed
    /// (sigs threshold met). This is the local-only predicate; the slot
    /// may still be missing ancestors. Use [`Self::is_fed`] for the
    /// stronger ancestrally-ready predicate.
    pub fn is_confirmed(&self, round: Round, creator: PeerId) -> bool {
        self.units
            .get(&(round, creator))
            .is_some_and(|e| e.is_confirmed(self.threshold()))
    }

    /// Whether the slot at `(round, creator)` has been promoted: it is
    /// confirmed locally *and* every parent slot is also fed. Equivalent
    /// to "this slot has been handed to the extender, and its full
    /// causal closure is locally present and confirmed".
    pub fn is_fed(&self, round: Round, creator: PeerId) -> bool {
        self.fed.contains(&(round, creator))
    }

    /// Iterate the slots at `round` in `creator`-order.
    fn round_units(&self, round: Round) -> impl Iterator<Item = &Entry<D>> {
        self.units
            .range((round, PeerId::from(0u8))..)
            .take_while(move |((r, _), _)| *r == round)
            .map(|(_, e)| e)
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

    /// Insert a freshly-received unit (with the carried co-signatures)
    /// into the graph and return the inserted entry on a *fresh* insert.
    ///
    /// First-seen wins per slot — if we already hold a unit at
    /// `(unit.round, unit.creator)` the body is dropped, but the carried
    /// sigs are still merged into the existing slot (drop + merge is
    /// safe: an honest peer never co-signs two distinct units at the
    /// same slot, so a forker can't reach threshold on either side).
    ///
    /// Insert is *lax* on ancestry: a fresh unit lands as soon as it's
    /// well-formed (correct cardinality of parent creators, all drawn
    /// from the federation). Whether its parents are locally confirmed
    /// is *not* checked here — that gate has moved to
    /// [`Self::try_promote`], which feeds the extender and permits the
    /// slot to be used as a parent for our own future units. Accepting
    /// at any round is what lets a node restarting from empty state
    /// catch up by receiving a head-of-DAG push and then walking back
    /// via demand-pull on missing parents.
    ///
    /// On the fresh path the caller's `sig` is trusted (engine
    /// pre-verifies it against `unit.creator`); each entry in `cosigs`
    /// is re-verified here against the body and silently dropped on
    /// failure. Duplicate path drops the new body (first-seen wins) but
    /// merges any carried `cosigs` via `record_cosig`. Returns
    /// `Some(entry)` only on a fresh insert — that's the caller's
    /// signal to rebroadcast — `None` on duplicates and on any of the
    /// rejection paths above.
    pub fn insert_unit(
        &mut self,
        unit: Unit<D>,
        sig: schnorr::Signature,
        cosigs: BTreeMap<PeerId, schnorr::Signature>,
        keychain: &Keychain,
    ) -> Option<Entry<D>> {
        if self.units.contains_key(&(unit.round, unit.creator)) {
            for (signer, cosig) in cosigs {
                self.record_cosig(unit.round, unit.creator, signer, cosig, keychain);
            }
            return None;
        }

        // Re-encode the payload and reject anything past the byte cap.
        // `D` is generic, so the bound has to be checked here rather
        // than at decode time — a malicious creator that bundles too
        // many items into one unit gets dropped before we keep the
        // entry around.
        if unit.data.consensus_encode_to_vec().len() > BFT_UNIT_BYTE_LIMIT {
            return None;
        }

        self.check_parents(&unit).ok()?;

        // Filter cosigs that don't verify or come from the creator.
        // The creator's signature lives in the dedicated `sig` field;
        // a self-cosig from the creator would be redundant.
        let session = self.session;
        let valid_cosigs: BTreeMap<PeerId, schnorr::Signature> = cosigs
            .into_iter()
            .filter(|(signer, c)| {
                *signer != unit.creator && keychain.verify(&(session, &unit), c, *signer)
            })
            .collect();

        let entry = Entry::new(unit.clone(), sig, valid_cosigs);

        self.backup.save(&entry);

        self.units.insert((unit.round, unit.creator), entry.clone());

        self.try_promote(unit.round, unit.creator);

        Some(entry)
    }

    /// Install (or overwrite) a slot's entry from a `SignedUnit`-shaped
    /// bundle. The bundle must carry threshold-many valid signatures
    /// (`1 + cosigs.len() >= threshold`); if so, this is *cryptographic
    /// proof* that this body is the canonical one for the slot, and
    /// any previously-stored body at this slot was either the same
    /// body or a forker's stuck-below-threshold artifact that we can
    /// safely discard.
    ///
    /// Returns `true` iff the bundle verified and was installed.
    /// Returns `false` for: session mismatch, oversize payload,
    /// malformed parents, invalid creator sig, or insufficient valid
    /// cosigs after filtering.
    pub fn insert_signed_unit(
        &mut self,
        unit: Unit<D>,
        sig: schnorr::Signature,
        cosigs: BTreeMap<PeerId, schnorr::Signature>,
        keychain: &Keychain,
    ) -> bool {
        if unit.data.consensus_encode_to_vec().len() > BFT_UNIT_BYTE_LIMIT {
            return false;
        }

        let session = self.session;

        if !keychain.verify(&(session, &unit), &sig, unit.creator) {
            return false;
        }

        // Take exactly `2f = threshold - 1` valid non-creator cosigs.
        // BTreeMap iterates in PeerId order so this is deterministic
        // across peers; the iterator short-circuits, so any extra
        // cosigs in the bundle are never verified (saves CPU).
        // Structural parent validity (cardinality + federation
        // membership) doesn't need a separate check here — at least
        // f+1 of the verified sigs come from honest peers, who ran
        // `check_parents` before signing, so the threshold proof
        // implies the parent set is well-formed.
        let valid_cosigs: BTreeMap<PeerId, schnorr::Signature> = cosigs
            .into_iter()
            .filter(|(signer, c)| {
                *signer != unit.creator && keychain.verify(&(session, &unit), c, *signer)
            })
            .take(self.threshold() - 1)
            .collect();

        if 1 + valid_cosigs.len() != self.threshold() {
            return false;
        }

        let entry = Entry::new(unit.clone(), sig, valid_cosigs);

        self.backup.save(&entry);

        // Atomic install/overwrite. A previously-promoted slot can
        // only be re-installed with the same body (quorum math forbids
        // two distinct bodies reaching threshold), so this never
        // invalidates an extender promotion.
        self.units.insert((unit.round, unit.creator), entry);

        self.try_promote(unit.round, unit.creator);

        true
    }

    /// Record a co-signature on the unit at `(round, creator)`.
    ///
    /// The signature is verified against the body *we currently hold*
    /// at that slot — the consistent-broadcast safety check, since a
    /// forker trying to split co-signers across two distinct bodies
    /// will find that each peer's collected cosigs only verify against
    /// their local body. Stale cosigs (verify failure, dupe signer,
    /// already-confirmed slot, signer == creator) are silently
    /// discarded.
    ///
    /// Returns `true` iff we hold the body locally (regardless of
    /// whether the cosig was new). Returns `false` if we don't hold
    /// the body — the caller is expected to demand-pull from the
    /// signer in that case.
    pub fn record_cosig(
        &mut self,
        round: Round,
        creator: PeerId,
        signer: PeerId,
        sig: schnorr::Signature,
        keychain: &Keychain,
    ) -> bool {
        let t = self.threshold();

        let Some(entry) = self.units.get_mut(&(round, creator)) else {
            return false;
        };

        // Creator's sig already lives in `entry.sig`; a "cosig" from
        // the creator is meaningless. Treat as no-op success — we
        // *do* hold the body.
        if signer == creator {
            return true;
        }

        if entry.is_confirmed(t) {
            return true;
        }

        if entry.cosigs.contains_key(&signer) {
            return true;
        }

        if !keychain.verify(&(self.session, &entry.unit), &sig, signer) {
            return true;
        }

        entry.cosigs.insert(signer, sig);

        self.backup.save(entry);

        self.try_promote(round, creator);

        true
    }

    /// Try to promote the slot at `(round, creator)` into the extender;
    /// if it does promote, sweep ascending rounds promoting whatever
    /// became newly eligible, stopping when a full round produces zero
    /// promotions.
    ///
    /// Promotion conditions for one slot:
    /// 1. Not already in `fed`.
    /// 2. Confirmed (sigs ≥ threshold).
    /// 3. Round 0, or every parent slot is already in `fed`.
    ///
    /// Termination: a round can only gain newly-promotable slots if the
    /// previous round did. Once a sweep promotes nothing, no higher
    /// round can promote anything either, so we stop.
    fn try_promote(&mut self, round: Round, creator: PeerId) {
        if !self.maybe_promote(round, creator) {
            return;
        }

        let mut next_round = round.saturating_add(1);

        loop {
            let candidates: Vec<PeerId> = self
                .round_units(next_round)
                .map(|e| e.unit.creator)
                .collect();

            let mut any_promoted = false;
            for c in candidates {
                if self.maybe_promote(next_round, c) {
                    any_promoted = true;
                }
            }

            if !any_promoted {
                return;
            }

            next_round = next_round.saturating_add(1);
        }
    }

    /// Attempt to promote one slot. Returns `true` iff the slot
    /// transitioned from unfed to fed in this call.
    fn maybe_promote(&mut self, round: Round, creator: PeerId) -> bool {
        if self.fed.contains(&(round, creator)) {
            return false;
        }

        let Some(entry) = self.units.get(&(round, creator)) else {
            return false;
        };

        if !entry.is_confirmed(self.threshold()) {
            return false;
        }

        if let Some(parent_round) = round.checked_sub(1) {
            let parents_fed = entry
                .unit
                .parents
                .iter()
                .all(|p| self.fed.contains(&(parent_round, *p)));
            if !parents_fed {
                return false;
            }
        }

        let unit = entry.unit.clone();
        self.fed.insert((round, creator));
        self.extender.add_unit(unit);

        true
    }

    /// Build a candidate parent set for a unit at `round`.
    ///
    /// For `round == 0`, returns an empty parent set unconditionally —
    /// round-0 units are the DAG's roots.
    ///
    /// For `round > 0`, returns the lowest-`PeerId`-keyed `threshold`
    /// *fed* slots at `round - 1`, or `None` if fewer than `threshold`
    /// slots at that round have been promoted yet. Parent hashes aren't
    /// tracked: at most one unit per slot can confirm, so the creator
    /// suffices.
    ///
    /// Parents must be `fed` (ancestrally ready) rather than just
    /// confirmed: building a unit atop a parent whose own ancestors
    /// aren't ready would mean the new unit would also fail to promote
    /// — wasted work and lost rebroadcast opportunity.
    ///
    /// No chain rule: a creator's own previous-round unit is *not*
    /// forced into the parent set. Recovery is independent of the
    /// chain — the periodic anti-entropy push refills sig deficits at
    /// slots receivers already hold, and the per-creator pull
    /// (`Message::Request` for the lowest unconfirmed round) pulls in
    /// missing units one round at a time.
    pub fn parents_for(&self, round: Round) -> Option<BTreeSet<PeerId>> {
        let Some(parent_round) = round.checked_sub(1) else {
            return Some(BTreeSet::new());
        };

        let t = self.threshold();

        let parents: BTreeSet<PeerId> = self
            .round_units(parent_round)
            .filter(|e| self.fed.contains(&(parent_round, e.unit.creator)))
            .take(t)
            .map(|e| e.unit.creator)
            .collect();

        (parents.len() == t).then_some(parents)
    }

    /// Structural parent check used by `insert_unit`. Returns `Err(())`
    /// when the parent set is malformed (wrong cardinality, or names a
    /// creator outside the federation); `Ok(())` otherwise.
    ///
    /// - Round 0 must have an empty parent set.
    /// - Round R>0 must carry exactly `threshold` parent creators.
    /// - Every parent creator must be a member of the federation.
    ///
    /// Local presence/confirmation of the parent slots is *not* checked
    /// here — that's the promotion gate, enforced by `try_promote` and
    /// `parents_for`.
    fn check_parents(&self, unit: &Unit<D>) -> Result<(), ()> {
        let t = self.threshold();

        if unit.round == 0 {
            return if unit.parents.is_empty() {
                Ok(())
            } else {
                Err(())
            };
        }

        if unit.parents.len() != t {
            return Err(());
        }

        for p_creator in &unit.parents {
            if !self.n.peer_ids().any(|p| p == *p_creator) {
                return Err(());
            }
        }

        Ok(())
    }
}
