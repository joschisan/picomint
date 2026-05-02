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

/// One slot in the DAG: the unit body, the creator's signature, and
/// the co-signatures collected so far.
#[derive(Debug, Clone, Encodable, Decodable)]
pub struct Entry<D: UnitData> {
    unit: Unit<D>,
    sig: schnorr::Signature,
    cosigs: BTreeMap<PeerId, schnorr::Signature>,
}

// `Entry<D>` is a redb value here so callers can use it directly —
// the orphan rule blocks downstream `impl Value` blocks.
picomint_redb::consensus_value!([D: UnitData] Entry<D>);

impl<D: UnitData> Entry<D> {
    fn new(
        unit: Unit<D>,
        sig: schnorr::Signature,
        cosigs: BTreeMap<PeerId, schnorr::Signature>,
    ) -> Self {
        Self { unit, sig, cosigs }
    }

    pub fn unit(&self) -> &Unit<D> {
        &self.unit
    }

    pub fn sig(&self) -> &schnorr::Signature {
        &self.sig
    }

    pub fn cosigs(&self) -> &BTreeMap<PeerId, schnorr::Signature> {
        &self.cosigs
    }

    /// Local sig threshold met. Does *not* imply ancestors are ready —
    /// see [`Graph::is_extended`].
    pub fn is_confirmed(&self, threshold: usize) -> bool {
        1 + self.cosigs.len() >= threshold
    }
}

/// Per-peer view of the consensus DAG plus its persistence and
/// ordering machinery. See `README.md` for the algorithm; the two
/// load-bearing invariants are:
///
/// - At most one body per `(round, creator)` slot can ever reach
///   threshold (consistent broadcast).
/// - A slot is in `extended` iff it's confirmed locally and every
///   parent slot is also in `extended`.
pub struct Graph<D: UnitData> {
    session: u64,
    n: NumPeers,
    units: BTreeMap<(Round, PeerId), Entry<D>>,
    extended: BTreeSet<(Round, PeerId)>,
    backup: DynBackup<D>,
    extender: Extender<D>,
}

impl<D: UnitData> Graph<D> {
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
            extended: BTreeSet::new(),
            backup,
            extender,
        };

        // `Backup::load`'s (round, peer) lex order is a valid
        // topological order, so inserting and extending in one pass
        // works — every parent of (R, c) has already been processed.
        for entry in graph.backup.load() {
            let r = entry.unit.round;
            let c = entry.unit.creator;
            graph.units.insert((r, c), entry);
            graph.try_extend(r, c);
        }

        graph
    }

    pub fn session(&self) -> u64 {
        self.session
    }

    pub fn threshold(&self) -> usize {
        self.n.threshold()
    }

    pub fn peer_ids(&self) -> impl Iterator<Item = PeerId> {
        self.n.peer_ids()
    }

    pub fn entry(&self, round: Round, creator: PeerId) -> Option<&Entry<D>> {
        self.units.get(&(round, creator))
    }

    /// Local sigs threshold met. Does *not* imply ancestors are ready.
    pub fn is_confirmed(&self, round: Round, creator: PeerId) -> bool {
        self.units
            .get(&(round, creator))
            .is_some_and(|e| e.is_confirmed(self.threshold()))
    }

    /// Confirmed *and* every parent slot is extended.
    pub fn is_extended(&self, round: Round, creator: PeerId) -> bool {
        self.extended.contains(&(round, creator))
    }

    fn round_units(&self, round: Round) -> impl Iterator<Item = &Entry<D>> {
        self.units
            .range((round, PeerId::from(0u8))..)
            .take_while(move |((r, _), _)| *r == round)
            .map(|(_, e)| e)
    }

    pub fn highest_entry(&self, creator: PeerId) -> Option<&Entry<D>> {
        self.units
            .iter()
            .rev()
            .find_map(|((_, c), e)| (*c == creator).then_some(e))
    }

    /// Lax body insert. Caller pre-verifies `sig`; each entry in
    /// `cosigs` is re-verified against the body, invalid ones dropped.
    /// Duplicate slot: first-seen body wins, carried cosigs merge via
    /// `record_cosig`. Returns `Some(entry)` only on fresh insert (the
    /// caller's signal to rebroadcast).
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

        // `D` is generic, so the byte cap has to be enforced here at
        // re-encode time rather than at decode time.
        if unit.data.consensus_encode_to_vec().len() > BFT_UNIT_BYTE_LIMIT {
            return None;
        }

        self.check_parents(&unit).ok()?;

        let session = self.session;
        let valid_cosigs: BTreeMap<PeerId, schnorr::Signature> = cosigs
            .into_iter()
            .filter(|(signer, c)| {
                *signer != unit.creator && keychain.verify(session, &unit, c, *signer)
            })
            .collect();

        let entry = Entry::new(unit.clone(), sig, valid_cosigs);

        self.backup.save(&entry);

        self.units.insert((unit.round, unit.creator), entry.clone());

        self.try_extend(unit.round, unit.creator);

        Some(entry)
    }

    /// Install (or overwrite) a slot from a threshold-proven bundle.
    /// Returns `true` iff the bundle verified and was installed. A
    /// valid `SignedUnit` proves canonical body — quorum math forbids
    /// two distinct bodies reaching threshold — so overwrite is safe.
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

        if !keychain.verify(session, &unit, &sig, unit.creator) {
            return false;
        }

        // BTreeMap's PeerId-order iteration + `take(t-1)` short-circuit
        // means extras past 2f are never verified. Structural parent
        // validity is implied by the threshold proof (≥ f+1 honest
        // signers ran `check_parents` before signing).
        let valid_cosigs: BTreeMap<PeerId, schnorr::Signature> = cosigs
            .into_iter()
            .filter(|(signer, c)| {
                *signer != unit.creator && keychain.verify(session, &unit, c, *signer)
            })
            .take(self.threshold() - 1)
            .collect();

        if 1 + valid_cosigs.len() != self.threshold() {
            return false;
        }

        let entry = Entry::new(unit.clone(), sig, valid_cosigs);

        self.backup.save(&entry);

        self.units.insert((unit.round, unit.creator), entry);

        self.try_extend(unit.round, unit.creator);

        true
    }

    /// Verify a cosig against the body we hold and merge it. Returns
    /// `false` iff we don't hold the body — caller demand-pulls from
    /// the signer in that case. Stale cosigs (verify failure, dupe,
    /// already-confirmed, signer == creator) silently no-op.
    ///
    /// Verifying against the *locally-held* body is the consistent-
    /// broadcast check: a forker's cosigs over a different body don't
    /// verify against ours, so neither side reaches threshold.
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

        if signer == creator {
            return true;
        }

        if entry.is_confirmed(t) {
            return true;
        }

        if entry.cosigs.contains_key(&signer) {
            return true;
        }

        if !keychain.verify(self.session, &entry.unit, &sig, signer) {
            return true;
        }

        entry.cosigs.insert(signer, sig);

        self.backup.save(entry);

        self.try_extend(round, creator);

        true
    }

    /// Extend `(round, creator)` if eligible, then sweep ascending
    /// rounds while each sweep produces at least one new extension.
    /// Termination is by induction — a round can only gain extensions
    /// when the previous one did.
    fn try_extend(&mut self, round: Round, creator: PeerId) {
        if !self.maybe_extend(round, creator) {
            return;
        }

        let mut next_round = round.saturating_add(1);

        loop {
            let candidates: Vec<PeerId> = self
                .round_units(next_round)
                .map(|e| e.unit.creator)
                .collect();

            let mut any_extended = false;
            for c in candidates {
                if self.maybe_extend(next_round, c) {
                    any_extended = true;
                }
            }

            if !any_extended {
                return;
            }

            next_round = next_round.saturating_add(1);
        }
    }

    /// Returns `true` iff this call transitioned the slot to extended.
    fn maybe_extend(&mut self, round: Round, creator: PeerId) -> bool {
        if self.extended.contains(&(round, creator)) {
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
                .all(|p| self.extended.contains(&(parent_round, *p)));
            if !parents_fed {
                return false;
            }
        }

        let unit = entry.unit.clone();
        self.extended.insert((round, creator));
        self.extender.add_unit(unit);

        true
    }

    /// Lowest-`PeerId`-keyed `threshold` extended slots at `round-1`,
    /// or `None` if fewer than `threshold` slots there are extended.
    /// Empty set for round 0. Filtering by `extended` (not `confirmed`)
    /// guarantees any unit we author is itself extendable on receivers.
    pub fn parents_for(&self, round: Round) -> Option<BTreeSet<PeerId>> {
        let Some(parent_round) = round.checked_sub(1) else {
            return Some(BTreeSet::new());
        };

        let t = self.threshold();

        let parents: BTreeSet<PeerId> = self
            .round_units(parent_round)
            .filter(|e| self.extended.contains(&(parent_round, e.unit.creator)))
            .take(t)
            .map(|e| e.unit.creator)
            .collect();

        (parents.len() == t).then_some(parents)
    }

    /// Structural-only parent check: round 0 has empty parents; round
    /// R>0 has exactly `threshold` parent creators all in the federation.
    /// Local presence of the parent slots is the extension gate's job.
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
