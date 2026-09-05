use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::{Result, ensure};
use async_channel::Sender;
use picomint_core::{NodeId, NumNodes};
use picomint_encoding::Encodable;
use picomint_redb::{Database, DbRead, Table, WriteTx};
use tokio::time::{Instant, sleep_until};
use tracing::warn;

use crate::data::DataProvider;
use crate::keychain::Keychain;
use crate::network::{INetwork, Message, Recipient};
use crate::unit::{Round, Unit, UnitData, UnitEnvelope, UnitHash};
use crate::view::{UnitView, View, merge_element, merge_views};

/// Periodic own-unit push interval. Pull is demand-driven, not periodic.
const ANTI_ENTROPY_INTERVAL: Duration = Duration::from_secs(1);

/// Minimum interval between successive `Request` sends for the same
/// unit. Caps the parent-walk fan-out so anti-entropy retransmits of
/// the same top-of-chain unit don't keep re-firing the whole tree of
/// requests every second.
const REQUEST_DEDUP_INTERVAL: Duration = Duration::from_secs(1);

/// Drives a single node's growth indefinitely. The caller constructs
/// the engine, then awaits `run()` (typically in a spawned task) and
/// keeps the receiving end of `ordered_tx` for items as they commit.
///
/// On startup `run()` replays ``"bft-units"`` through `try_extend` +
/// `run_extender` to rebuild the in-memory `rounds` / `extended` /
/// `emitted` / `next_decide_round` and re-emit every
/// previously-committed item through `ordered_tx`. The caller-side
/// idempotency check (e.g. the daemon's `resume_from` cursor over its
/// accepted-items table) absorbs the redelivery.
pub struct Engine<P, D, T, N>
where
    D: UnitData,
    P: DataProvider<D>,
    T: Table<Key = UnitHash, Value = UnitEnvelope<D>>,
    N: INetwork<D>,
{
    pub(crate) id: NodeId,
    session: u32,
    pub(crate) n: NumNodes,
    db: Database,
    keychain: Keychain,
    network: N,
    data_provider: P,
    pub(crate) ordered_tx: Sender<(Round, NodeId, D)>,

    /// Daemon-declared units table (`UnitHash => UnitEnvelope<D>`).
    /// Bft only reads/writes it.
    pub(crate) units_table: T,

    /// Hashes of every stored unit keyed by round — the round index
    /// over `units_table`. Drives the extension cascade and, filtered
    /// by `extended` membership, the extender's per-round scans.
    /// Rebuilt from disk on startup; appended in `insert_unit`.
    pub(crate) rounds: BTreeMap<Round, BTreeSet<UnitHash>>,
    /// Units whose envelope is present in `units_table` *and* whose
    /// every parent is itself in this map, each holding its bare
    /// [`Unit`]. The units are the complete evidence the commit rule
    /// tallies over, so the extender never touches the db except to
    /// read payloads at emission. Rebuilt from disk on startup; never
    /// persisted.
    pub(crate) extended: BTreeMap<UnitHash, Unit>,
    /// Units whose payload has been sent through `ordered_tx`.
    /// Prevents re-emission across batches and within one BFS.
    pub(crate) emitted: BTreeSet<UnitHash>,
    /// Extender cursor: the next round whose head to attempt deciding.
    pub(crate) next_decide_round: Round,
    /// Include/exclude decisions already reached per candidate, kept
    /// for the engine's lifetime. Sound because decisions propagate:
    /// one deciding unit forces its whole round and every round above
    /// to vote its value (see [`crate::extender`]).
    pub(crate) decided: BTreeMap<UnitHash, bool>,
    /// Memoized virtual votes, keyed by `(candidate, voter)` and kept
    /// for the engine's lifetime. A vote is a pure function of the
    /// voter's fixed ancestry, so caching never goes stale.
    pub(crate) votes: BTreeMap<(UnitHash, UnitHash), bool>,
    /// Rounds of our own units that carry items and whose unit is not
    /// yet in `emitted`. Seeded from disk on startup; drained by the
    /// extender as units emit.
    pub(crate) unordered_own_data: BTreeSet<Round>,
    /// Round and hash of our own highest unit; the base for next-round
    /// creation and the anti-entropy push. Seeded from disk on
    /// startup; advanced in `insert_unit`.
    own_top: Option<(Round, UnitHash)>,
    /// Last time we sent `Message::Request` for a given unit. Used to
    /// throttle re-asks so anti-entropy retransmits don't fan out
    /// duplicate parent-walks every tick.
    request_sent_at: BTreeMap<UnitHash, Instant>,
    /// Per-extended-unit ancestry views — inputs to the reference
    /// rule (see [`crate::view`]). Computed at extension from the
    /// parents' memoized views and, like `votes`, kept for the
    /// engine's lifetime.
    views: BTreeMap<UnitHash, UnitView>,
    /// Units refused by the reference rule — they pin a creator their
    /// other parents prove forked. Never extended; cached so the
    /// cascade doesn't re-judge them on every sweep.
    invalid: BTreeSet<UnitHash>,
}

impl<P, D, T, N> Engine<P, D, T, N>
where
    D: UnitData,
    P: DataProvider<D>,
    T: Table<Key = UnitHash, Value = UnitEnvelope<D>>,
    N: INetwork<D>,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: NodeId,
        session: u32,
        n: NumNodes,
        db: Database,
        keychain: Keychain,
        network: N,
        data_provider: P,
        ordered_tx: Sender<(Round, NodeId, D)>,
        units_table: T,
    ) -> Self {
        Self {
            id,
            session,
            n,
            db,
            keychain,
            network,
            data_provider,
            ordered_tx,
            units_table,
            rounds: BTreeMap::new(),
            extended: BTreeMap::new(),
            emitted: BTreeSet::new(),
            next_decide_round: 0,
            decided: BTreeMap::new(),
            votes: BTreeMap::new(),
            unordered_own_data: BTreeSet::new(),
            own_top: None,
            request_sent_at: BTreeMap::new(),
            views: BTreeMap::new(),
            invalid: BTreeSet::new(),
        }
    }

    pub async fn run(mut self) {
        self.replay().await;

        self.create_units().await;

        let mut next_anti_entropy_at = Instant::now();

        loop {
            tokio::select! {
                maybe_msg = self.network.receive() => {
                    let Some((sender, msg)) = maybe_msg else { return };

                    match self.handle_message(sender, msg).await {
                        Ok(()) => self.create_units().await,
                        Err(err) => {
                            warn!(%sender, err = %format_args!("{err:#}"), "rejected bft message");
                        }
                    }
                }

                _ = self.data_provider.wait_for_data() => {
                    self.create_units().await;
                }

                _ = sleep_until(next_anti_entropy_at) => {
                    self.broadcast_anti_entropy();

                    next_anti_entropy_at = Instant::now() + ANTI_ENTROPY_INTERVAL;
                }
            }
        }
    }

    /// Create our own units for every round that has become creatable,
    /// stopping when the next round lacks a threshold of extended
    /// parents. Within that window creation is gated on having work: a
    /// unit is only built if it would carry items or an earlier unit of
    /// ours still awaits ordering. With no work pending the engine goes
    /// quiescent until `wait_for_data` resolves or an inbound message
    /// arrives.
    async fn create_units(&mut self) {
        while self.try_create_unit().await {}
    }

    /// Rebuild the in-memory `rounds` / `extended` / `emitted` /
    /// `next_decide_round` / `own_top` from persisted ``"bft-units"``, and
    /// re-emit every committed item through `ordered_tx`.
    ///
    /// Correctness rests on determinism: `try_extend` is a fixpoint over
    /// the parent-extended predicate, and the extender's vote tally +
    /// `bfs_batch` are both deterministic over the final unit set.
    /// So calling `try_extend` for every round-zero unit (the cascade
    /// root) and then `run_extender` once produces the same `extended`
    /// set and the same channel emission sequence as the live
    /// unit-by-unit growth did before the restart.
    async fn replay(&mut self) {
        let dbtx = self.db.begin_read();

        let units: Vec<(UnitHash, Unit)> = dbtx.iter(&self.units_table, |it| {
            it.map(|(hash, ev)| (hash, ev.unit)).collect()
        });

        for (hash, unit) in units {
            self.rounds.entry(unit.round).or_default().insert(hash);

            if unit.creator == self.id {
                if unit.data.is_some() {
                    self.unordered_own_data.insert(unit.round);
                }

                if self.own_top.is_none_or(|(top, _)| unit.round > top) {
                    self.own_top = Some((unit.round, hash));
                }
            }
        }

        for hash in self.round_units(0) {
            self.try_extend(&dbtx, hash);
        }

        self.run_extender(&dbtx).await;
    }

    /// One write tx per inbound message; on Ok commit it, on Err drop
    /// it (any partial writes roll back). All reads in handlers see
    /// their own writes via `WriteTx`'s read-your-own-writes.
    /// In-memory mutations (`rounds`, `extended`, `emitted`, channel
    /// sends) are not rolled back on Err — only the persistent
    /// ``"bft-units"`` writes are. The mutators only run after the dbtx
    /// writes succeed via `?`.
    ///
    /// These commits use **relaxed** (non-fsync) durability: inbound units
    /// are node-originated and re-fetched via anti-entropy after a crash, so
    /// they need not be individually durable. The fsync barrier is
    /// [`Self::try_create_unit`], whose durable commit before broadcast both
    /// prevents our own equivocation and flushes this relaxed backlog.
    async fn handle_message(&mut self, sender: NodeId, msg: Message<D>) -> Result<()> {
        match msg {
            Message::Unit(ev) => {
                let dbtx = self.db.begin_write_relaxed();

                // Pull missing ancestors before the install attempt so
                // a duplicate unit (rejected below) still re-fires the
                // walk — that retry is what heals dropped Requests.
                self.cascade_parents(&dbtx, sender, &ev.unit);

                let hash = ev.unit.hash();

                self.insert_unit(&dbtx, &ev, hash)?;

                self.try_extend(&dbtx, hash);
                self.run_extender(&dbtx).await;

                dbtx.commit();
            }
            Message::Request(hash) => {
                self.handle_request(&self.db.begin_read(), sender, hash);
            }
        }

        Ok(())
    }

    fn broadcast_anti_entropy(&self) {
        let Some((_, hash)) = self.own_top else {
            return;
        };

        let ev = self
            .db
            .begin_read()
            .get(&self.units_table, &hash)
            .expect("own top unit is stored");

        self.network.send(Recipient::Everyone, Message::Unit(ev));
    }

    /// Send `Message::Request` for `hash` to `node`, but only if we
    /// haven't asked for the same unit within the past
    /// [`REQUEST_DEDUP_INTERVAL`]. Anti-entropy retransmits the same
    /// top-of-chain unit every second, and every receipt would
    /// otherwise refire the entire ancestor walk — so we throttle the
    /// outgoing request rate per unit to one per cache window.
    fn try_send_request(&mut self, node: NodeId, hash: UnitHash) {
        let now = Instant::now();

        if self
            .request_sent_at
            .get(&hash)
            .filter(|prev| now.duration_since(**prev) < REQUEST_DEDUP_INTERVAL)
            .is_some()
        {
            return;
        }

        self.request_sent_at.insert(hash, now);

        self.network
            .send(Recipient::Node(node), Message::Request(hash));
    }

    /// Walk ancestors of `top` locally and `Request` only the missing
    /// frontier from `sender`. We descend through every
    /// present-but-not-extended ancestor because we already hold its
    /// parent set; units whose bodies we lack are requested and
    /// terminate the walk, as do extended units and units we've
    /// already requested recently (via `try_send_request`).
    fn cascade_parents(&mut self, dbtx: &impl DbRead, sender: NodeId, top: &Unit) {
        let mut visited: BTreeSet<UnitHash> = BTreeSet::new();
        let mut stack: Vec<UnitHash> = top.parents.values().copied().collect();

        while let Some(hash) = stack.pop() {
            if !visited.insert(hash) {
                continue;
            }

            if self.is_extended(hash) {
                continue;
            }

            let Some(ev) = dbtx.get(&self.units_table, &hash) else {
                self.try_send_request(sender, hash);
                continue;
            };

            stack.extend(ev.unit.parents.values().copied());
        }
    }

    /// Reply with the stored envelope; no reply if we don't hold the
    /// unit.
    fn handle_request(&self, dbtx: &impl DbRead, requester: NodeId, hash: UnitHash) {
        let Some(ev) = dbtx.get(&self.units_table, &hash) else {
            return;
        };

        self.network
            .send(Recipient::Node(requester), Message::Unit(ev));
    }

    /// Validate and install a fresh unit envelope in ``"bft-units"`` under
    /// `hash` (its unit hash, computed by the caller), then index it
    /// in `rounds` and advance `own_top` for our own units. A
    /// duplicate unit hits the same key and errors.
    fn insert_unit(&mut self, dbtx: &WriteTx, ev: &UnitEnvelope<D>, hash: UnitHash) -> Result<()> {
        // Before the signature check, which looks the creator up in the
        // keychain — an out-of-mint creator has no key there.
        ensure!(
            self.n.node_ids().any(|x| x == ev.unit.creator),
            "unit creator not in mint",
        );

        if ev.unit.round == 0 {
            ensure!(
                ev.unit.parents.is_empty(),
                "round 0 unit must have no parents",
            );
        } else {
            ensure!(
                ev.unit.parents.len() == self.n.threshold(),
                "non-zero round unit must have threshold parents",
            );

            for p in ev.unit.parents.keys() {
                ensure!(
                    self.n.node_ids().any(|x| x == *p),
                    "parent creator not in mint",
                );
            }
        }

        ensure!(
            self.keychain
                .verify(self.session, &ev.unit, &ev.sig, ev.unit.creator),
            "invalid creator signature",
        );

        // The signature covers only the unit, which pins the payload by
        // hash — so the envelope's payload must be checked against that
        // commitment or a third party relaying the unit could swap it.
        ensure!(
            ev.unit.data == (!ev.data.is_empty()).then(|| ev.data.consensus_hash_sha256()),
            "payload does not match the unit's data commitment",
        );

        ensure!(
            dbtx.insert(&self.units_table, &hash, ev).is_none(),
            "unit already stored",
        );

        self.rounds.entry(ev.unit.round).or_default().insert(hash);

        if ev.unit.creator == self.id && self.own_top.is_none_or(|(top, _)| ev.unit.round > top) {
            self.own_top = Some((ev.unit.round, hash));
        }

        Ok(())
    }

    async fn try_create_unit(&mut self) -> bool {
        let round = self.own_top.map_or(0, |(top, _)| top + 1);

        let Some(parents) = self.parents_for(round) else {
            return false;
        };

        let data: Vec<D> = self.data_provider.get_data();

        // Quiescence gate: only build a unit that carries items or keeps
        // the DAG growing while an earlier unit of ours awaits ordering.
        if data.is_empty() && !self.has_unordered_own_data() {
            return false;
        }

        let dbtx = self.db.begin_write();

        let unit = Unit {
            round,
            creator: self.id,
            parents,
            data: (!data.is_empty()).then(|| data.consensus_hash_sha256()),
        };

        let hash = unit.hash();

        let ev = UnitEnvelope {
            sig: self.keychain.sign(self.session, &unit),
            unit,
            data,
        };

        // Crash barrier: persist before broadcasting, otherwise a
        // restart would let us build a *different* unit at this round
        // from a fresh data_provider draw — nodes that saw the
        // original would consider us a forker.
        self.insert_unit(&dbtx, &ev, hash)
            .expect("newly built unit must insert");

        if ev.unit.data.is_some() {
            self.unordered_own_data.insert(round);
        }

        self.try_extend(&dbtx, hash);

        self.run_extender(&dbtx).await;

        dbtx.commit();

        self.network.send(Recipient::Everyone, Message::Unit(ev));

        true
    }

    // --- in-memory extension state ---

    /// True while any of our own units carries items that have not yet
    /// been emitted through `ordered_tx`.
    pub fn has_unordered_own_data(&self) -> bool {
        !self.unordered_own_data.is_empty()
    }

    /// Body present *and* every parent is extended.
    pub(crate) fn is_extended(&self, hash: UnitHash) -> bool {
        self.extended.contains_key(&hash)
    }

    /// The stored units of `round`.
    pub(crate) fn round_units(&self, round: Round) -> BTreeSet<UnitHash> {
        self.rounds.get(&round).cloned().unwrap_or_default()
    }

    /// Extend `hash` if eligible, then sweep ascending rounds while
    /// each sweep produces at least one new extension. Termination is
    /// by induction — a round can only gain extensions when the
    /// previous one did.
    pub(crate) fn try_extend(&mut self, dbtx: &impl DbRead, hash: UnitHash) {
        let Some(round) = self.maybe_extend(dbtx, hash) else {
            return;
        };

        let mut next_round = round.saturating_add(1);

        loop {
            let candidates = self.round_units(next_round);

            let mut any_extended = false;
            for candidate in candidates {
                if self.maybe_extend(dbtx, candidate).is_some() {
                    any_extended = true;
                }
            }

            if !any_extended {
                return;
            }

            next_round = next_round.saturating_add(1);
        }
    }

    /// Returns the unit's round iff this call transitioned it to
    /// extended. Every parent must be an extended unit created by the
    /// node it is keyed under at exactly `round - 1` — this pins each
    /// unit's claimed position to its parents' (inductively, down to
    /// round 0), so extended rounds are gap-free and a forker cannot
    /// key its own branches under other creators to fake the
    /// distinct-creator quorums the decision rule tallies. Round-0
    /// units have empty parent maps (enforced at insert), so their
    /// parent check is vacuously true. On top of the position check,
    /// every parent entry must pass the reference rule (see
    /// [`crate::view`]): a unit pinning a creator its other parents
    /// prove forked is invalid and never extends.
    fn maybe_extend(&mut self, dbtx: &impl DbRead, hash: UnitHash) -> Option<Round> {
        if self.is_extended(hash) || self.invalid.contains(&hash) {
            return None;
        }

        let ev = dbtx.get(&self.units_table, &hash)?;

        let parents_fed = ev.unit.parents.iter().all(|(creator, parent)| {
            self.extended
                .get(parent)
                .is_some_and(|p| p.creator == *creator && p.round + 1 == ev.unit.round)
        });

        if !parents_fed {
            return None;
        }

        if let Some(forked) = ev
            .unit
            .parents
            .keys()
            .find(|creator| self.pin_forked(&ev.unit.parents, **creator))
        {
            warn!(creator = %forked, unit = ?hash, "unit pins a forked creator; refusing to extend it");

            self.invalid.insert(hash);

            return None;
        }

        let view = self.compute_view(&ev.unit, hash);

        self.views.insert(hash, view);

        self.extended.insert(hash, ev.unit.clone());

        Some(ev.unit.round)
    }

    /// The reference-rule judgment for one parent entry: true iff the
    /// combined views of the *other* parents prove `creator` forked.
    /// The judged pin itself is excluded so the first unit whose
    /// parents span both fork branches — the merge point that carries
    /// the evidence upward — stays valid.
    fn pin_forked(&self, parents: &BTreeMap<NodeId, UnitHash>, creator: NodeId) -> bool {
        let mut judgment: Option<View> = None;

        for entry in parents.iter().filter(|entry| *entry.0 != creator) {
            let Some(view) = self.views.get(entry.1).and_then(|view| view.get(&creator)) else {
                continue;
            };

            judgment = Some(match judgment {
                None => *view,
                Some(so_far) => merge_element(&self.extended, creator, so_far, *view),
            });
        }

        judgment == Some(View::Forked)
    }

    /// A unit's view: its parents' views merged, plus the unit itself
    /// chained onto its own column — or `Forked` there if it does not
    /// extend the column tip its ancestry already contains, since two
    /// own units off one chain are themselves a fork.
    fn compute_view(&self, unit: &Unit, hash: UnitHash) -> UnitView {
        let mut view = UnitView::new();

        for parent in unit.parents.values() {
            let parent_view = self
                .views
                .get(parent)
                .expect("parents are extended and extended units have views");

            merge_views(&self.extended, &mut view, parent_view);
        }

        let own = match view.get(&unit.creator).copied() {
            None => View::Tip(unit.round, hash),
            Some(View::Forked) => View::Forked,
            Some(View::Tip(round, tip)) => {
                if unit.parents.get(&unit.creator) == Some(&tip) && round + 1 == unit.round {
                    View::Tip(unit.round, hash)
                } else {
                    View::Forked
                }
            }
        };

        view.insert(unit.creator, own);

        view
    }

    /// Our own unit at `round-1` plus the `threshold - 1` lowest-`NodeId`
    /// other creators' extended units (lowest-hash branch per forked
    /// creator), or `None` if our own unit isn't extended yet or fewer
    /// than `threshold` are. Empty map for round 0. Filtering by `extended` (not mere presence) guarantees
    /// any unit we author is itself extendable on receivers.
    ///
    /// The self-parent is a creation-side rule only — receivers don't
    /// verify it. It chains our column: every unit of ours is an
    /// ancestor of all our later units, so one downstream reference of
    /// our column emits our whole backlog.
    fn parents_for(&self, round: Round) -> Option<BTreeMap<NodeId, UnitHash>> {
        let Some(parent_round) = round.checked_sub(1) else {
            return Some(BTreeMap::new());
        };

        // A forker may have several extended branches; reference the
        // lowest-hash one. A creation-side choice only — receivers
        // don't verify which branch we picked.
        let mut extended_row: BTreeMap<NodeId, UnitHash> = BTreeMap::new();

        for hash in extended_at(&self.rounds, &self.extended, parent_round) {
            let extended = self.extended.get(&hash).expect("filtered on extended");

            extended_row.entry(extended.creator).or_insert(hash);
        }

        let own = *extended_row.get(&self.id)?;

        let t = self.n.threshold();

        // Mirror of the receivers' reference rule: drop any creator
        // the chosen row would judge forked and refill, so we never
        // sign a unit receivers refuse to extend. Each pass removes a
        // creator, so the loop ends.
        loop {
            let parents: BTreeMap<NodeId, UnitHash> = std::iter::once((self.id, own))
                .chain(
                    extended_row
                        .iter()
                        .filter(|entry| *entry.0 != self.id)
                        .map(|entry| (*entry.0, *entry.1)),
                )
                .take(t)
                .collect();

            if parents.len() < t {
                return None;
            }

            let Some(forked) = parents
                .keys()
                .find(|creator| self.pin_forked(&parents, **creator))
                .copied()
            else {
                return Some(parents);
            };

            if forked == self.id {
                return None;
            }

            extended_row.remove(&forked);
        }
    }
}

/// The extended units of `round`, in ascending hash order. Free over
/// the two engine fields it reads so the extender can walk it while
/// mutating the vote and decision caches.
pub(crate) fn extended_at<'a>(
    rounds: &'a BTreeMap<Round, BTreeSet<UnitHash>>,
    extended: &'a BTreeMap<UnitHash, Unit>,
    round: Round,
) -> impl Iterator<Item = UnitHash> + 'a {
    rounds
        .get(&round)
        .into_iter()
        .flatten()
        .copied()
        .filter(move |hash| extended.contains_key(hash))
}
