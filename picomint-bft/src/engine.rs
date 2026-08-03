use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::{Result, ensure};
use async_channel::Sender;
use picomint_core::config::BFT_UNIT_BYTE_LIMIT;
use picomint_core::{NumPeers, PeerId, peer_range};
use picomint_encoding::Encodable;
use picomint_redb::{Database, DbRead, Table, TableDef, WriteTx};
use tokio::time::{Instant, sleep_until};
use tracing::warn;

use crate::data::DataProvider;
use crate::keychain::Keychain;
use crate::network::{DynNetwork, Message, Recipient};
use crate::unit::{Round, SignedUnit, Unit, UnitData};

/// Periodic own-slot push interval. Pull is demand-driven, not periodic.
const ANTI_ENTROPY_INTERVAL: Duration = Duration::from_secs(1);

/// Minimum interval between successive `Request` sends for the same
/// `(round, creator)` slot. Caps the parent-walk fan-out so anti-entropy
/// retransmits of the same top-of-chain unit don't keep re-firing the
/// whole tree of requests every second.
const REQUEST_DEDUP_INTERVAL: Duration = Duration::from_secs(1);

/// Drives a single peer's growth indefinitely. The caller constructs
/// the engine, then awaits `run()` (typically in a spawned task) and
/// keeps the receiving end of `ordered_tx` for items as they commit.
///
/// On startup `run()` replays `BFT_UNITS` (in `(round, peer)` order)
/// through `try_extend` + `run_extender` to rebuild the in-memory
/// `extended` / `emitted` / `next_decide_round` and re-emit every
/// previously-committed item through `ordered_tx`. The caller-side
/// idempotency check (e.g. the daemon's `item_index` probe against
/// `ACCEPTED_ITEM`) absorbs the redelivery.
pub struct Engine<P, D>
where
    D: UnitData,
    P: DataProvider<D>,
{
    pub(crate) id: PeerId,
    session: u64,
    pub(crate) n: NumPeers,
    db: Database,
    keychain: Keychain,
    network: DynNetwork<D>,
    data_provider: P,
    pub(crate) ordered_tx: Sender<(Round, PeerId, D)>,

    /// Daemon-declared units table (`(Round, PeerId) => SignedUnit<D>`).
    /// Bft only reads/writes it.
    pub(crate) units_table: TableDef<(Round, PeerId), SignedUnit<D>>,

    /// Slots whose body is present in `units_table` *and* whose every
    /// parent slot is itself in this set. Rebuilt from disk on
    /// startup; never persisted.
    pub(crate) extended: BTreeSet<(Round, PeerId)>,
    /// Slots whose payload has been sent through `ordered_tx`.
    /// Prevents re-emission across batches and within one BFS.
    pub(crate) emitted: BTreeSet<(Round, PeerId)>,
    /// Extender cursor: the next leader round to attempt deciding.
    pub(crate) next_decide_round: Round,
    /// Rounds of our own units that carry items and whose slot is not
    /// yet in `emitted`. Seeded from disk on startup; drained by the
    /// extender as slots emit.
    pub(crate) unordered_own_data: BTreeSet<Round>,
    /// Last time we sent `Message::Request` for a given slot. Used to
    /// throttle re-asks so anti-entropy retransmits don't fan out
    /// duplicate parent-walks every tick.
    request_sent_at: BTreeMap<(Round, PeerId), Instant>,
}

impl<P, D> Engine<P, D>
where
    D: UnitData,
    P: DataProvider<D>,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new<TU>(
        id: PeerId,
        session: u64,
        n: NumPeers,
        db: Database,
        keychain: Keychain,
        network: DynNetwork<D>,
        data_provider: P,
        ordered_tx: Sender<(Round, PeerId, D)>,
        units_table: TU,
    ) -> Self
    where
        TU: Table<Key = (Round, PeerId), Value = SignedUnit<D>>,
    {
        Self {
            id,
            session,
            n,
            db,
            keychain,
            network,
            data_provider,
            ordered_tx,
            units_table: TableDef::from(units_table),
            extended: BTreeSet::new(),
            emitted: BTreeSet::new(),
            next_decide_round: 0,
            unordered_own_data: BTreeSet::new(),
            request_sent_at: BTreeMap::new(),
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

    /// Rebuild the in-memory `extended` / `emitted` / `next_decide_round`
    /// from persisted `BFT_UNITS`, and re-emit every committed item
    /// through `ordered_tx`.
    ///
    /// Correctness rests on determinism: `try_extend` is a fixpoint over
    /// the parent-extended predicate, and the extender's vote tally +
    /// `bfs_batch` are both deterministic over the final unit set.
    /// So calling `try_extend(0, c)` for every round-zero creator (the
    /// cascade root) and then `run_extender` once produces the same
    /// `extended` set and the same channel emission sequence as the
    /// live unit-by-unit growth did before the restart.
    async fn replay(&mut self) {
        let dbtx = self.db.begin_read();

        self.unordered_own_data = dbtx.iter(&self.units_table, |it| {
            it.filter(|((_, creator), signed)| *creator == self.id && !signed.unit.data.is_empty())
                .map(|((round, _), _)| round)
                .collect()
        });

        let round_zero: Vec<PeerId> = self
            .round_units(&dbtx, 0)
            .into_iter()
            .map(|u| u.creator)
            .collect();

        for creator in round_zero {
            self.try_extend(&dbtx, 0, creator);
        }

        self.run_extender(&dbtx).await;
    }

    /// One write tx per inbound message; on Ok commit it, on Err drop
    /// it (any partial writes roll back). All reads in handlers see
    /// their own writes via redb's read-your-own-writes on `WriteTx`.
    /// In-memory mutations (`extended`, `emitted`, channel sends) are
    /// not rolled back on Err — only the persistent `BFT_UNITS`
    /// writes are. The mutators only run after the dbtx writes
    /// succeed via `?`.
    ///
    /// These commits use **relaxed** (non-fsync) durability: inbound units
    /// are peer-originated and re-fetched via anti-entropy after a crash, so
    /// they need not be individually durable. The fsync barrier is
    /// [`Self::try_create_unit`], whose durable commit before broadcast both
    /// prevents our own equivocation and flushes this relaxed backlog.
    async fn handle_message(&mut self, sender: PeerId, msg: Message<D>) -> Result<()> {
        match msg {
            Message::Unit(signed) => {
                let dbtx = self.db.begin_write_relaxed();

                // Pull missing ancestors before the install attempt so
                // a duplicate unit (rejected below) still re-fires the
                // walk — that retry is what heals dropped Requests.
                self.cascade_parents(&dbtx, sender, &signed.unit);

                self.insert_unit(&dbtx, &signed)?;

                self.try_extend(&dbtx, signed.unit.round, signed.unit.creator);
                self.run_extender(&dbtx).await;

                dbtx.commit();
            }
            Message::Request { round, creator } => {
                self.handle_request(&self.db.begin_read(), sender, round, creator);
            }
        }

        Ok(())
    }

    fn broadcast_anti_entropy(&self) {
        let dbtx = self.db.begin_read();

        let Some(signed) = self.highest_unit(&dbtx, self.id) else {
            return;
        };

        self.network
            .send(Recipient::Everyone, Message::Unit(signed));
    }

    /// Send `Message::Request` for `(round, creator)` to `peer`, but
    /// only if we haven't asked for the same slot within the past
    /// [`REQUEST_DEDUP_INTERVAL`]. Anti-entropy retransmits the same
    /// top-of-chain unit every second, and every receipt would
    /// otherwise refire the entire ancestor walk — so we throttle the
    /// outgoing request rate per slot to one per cache window.
    fn try_send_request(&mut self, peer: PeerId, round: Round, creator: PeerId) {
        let now = Instant::now();

        if self
            .request_sent_at
            .get(&(round, creator))
            .filter(|prev| now.duration_since(**prev) < REQUEST_DEDUP_INTERVAL)
            .is_some()
        {
            return;
        }

        self.request_sent_at.insert((round, creator), now);

        self.network
            .send(Recipient::Peer(peer), Message::Request { round, creator });
    }

    /// Walk ancestors of `top` locally and `Request` only the missing
    /// frontier from `sender`. We descend through every
    /// present-but-not-extended ancestor because we already hold its
    /// parent set; slots whose bodies we lack are requested and
    /// terminate the walk, as do extended slots and slots we've
    /// already requested recently (via `try_send_request`).
    fn cascade_parents(&mut self, dbtx: &impl DbRead, sender: PeerId, top: &Unit<D>) {
        let mut visited: BTreeSet<(Round, PeerId)> = BTreeSet::new();
        let mut stack: Vec<(Round, PeerId)> = Vec::new();

        if let Some(parent_round) = top.round.checked_sub(1) {
            stack.extend(top.parents.iter().map(|c| (parent_round, *c)));
        }

        while let Some((round, creator)) = stack.pop() {
            if !visited.insert((round, creator)) {
                continue;
            }

            if self.is_extended(round, creator) {
                continue;
            }

            let Some(signed) = dbtx.get(&self.units_table, &(round, creator)) else {
                self.try_send_request(sender, round, creator);
                continue;
            };

            let Some(parent_round) = round.checked_sub(1) else {
                continue;
            };

            stack.extend(signed.unit.parents.iter().map(|c| (parent_round, *c)));
        }
    }

    /// Reply with the stored body and its creator sig; no reply if we
    /// don't hold the slot.
    fn handle_request(&self, dbtx: &impl DbRead, requester: PeerId, round: Round, creator: PeerId) {
        let Some(signed) = dbtx.get(&self.units_table, &(round, creator)) else {
            return;
        };

        self.network
            .send(Recipient::Peer(requester), Message::Unit(signed));
    }

    /// Validate and install a fresh `(round, creator)` slot in
    /// `BFT_UNITS`. An occupied slot errors — first body seen wins.
    fn insert_unit(&self, dbtx: &WriteTx, signed: &SignedUnit<D>) -> Result<()> {
        let unit = &signed.unit;

        ensure!(
            unit.data.consensus_encode_to_vec().len() <= BFT_UNIT_BYTE_LIMIT,
            "unit body exceeds size limit",
        );

        if unit.round == 0 {
            ensure!(unit.parents.is_empty(), "round 0 unit must have no parents");
        } else {
            ensure!(
                unit.parents.len() == self.n.threshold(),
                "non-zero round unit must have threshold parents",
            );

            for p in &unit.parents {
                ensure!(
                    self.n.peer_ids().any(|x| x == *p),
                    "parent creator not in federation",
                );
            }
        }

        ensure!(
            self.keychain
                .verify(self.session, unit, &signed.sig, unit.creator),
            "invalid creator signature",
        );

        ensure!(
            dbtx.insert(&self.units_table, &(unit.round, unit.creator), signed)
                .is_none(),
            "unit slot already occupied",
        );

        Ok(())
    }

    async fn try_create_unit(&mut self) -> bool {
        let round = self
            .highest_unit(&self.db.begin_read(), self.id)
            .map_or(0, |signed| signed.unit.round + 1);

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
            data,
        };

        let signed = SignedUnit {
            sig: self.keychain.sign(self.session, &unit),
            unit,
        };

        // Crash barrier: persist before broadcasting, otherwise a
        // restart would let us build a *different* unit at this slot
        // from a fresh data_provider draw — peers that saw the
        // original would consider us a forker.
        self.insert_unit(&dbtx, &signed)
            .expect("newly built unit must insert");

        if !signed.unit.data.is_empty() {
            self.unordered_own_data.insert(round);
        }

        self.try_extend(&dbtx, round, self.id);

        self.run_extender(&dbtx).await;

        dbtx.commit();

        self.network
            .send(Recipient::Everyone, Message::Unit(signed));

        true
    }

    // --- in-memory extension state ---

    /// True while any of our own units carries items that have not yet
    /// been emitted through `ordered_tx`.
    pub fn has_unordered_own_data(&self) -> bool {
        !self.unordered_own_data.is_empty()
    }

    /// Body present *and* every parent slot is extended.
    pub(crate) fn is_extended(&self, round: Round, creator: PeerId) -> bool {
        self.extended.contains(&(round, creator))
    }

    /// Extend `(round, creator)` if eligible, then sweep ascending
    /// rounds while each sweep produces at least one new extension.
    /// Termination is by induction — a round can only gain extensions
    /// when the previous one did.
    pub(crate) fn try_extend(&mut self, dbtx: &impl DbRead, round: Round, creator: PeerId) {
        if !self.maybe_extend(dbtx, round, creator) {
            return;
        }

        let mut next_round = round.saturating_add(1);

        loop {
            let candidates: Vec<PeerId> = self
                .round_units(dbtx, next_round)
                .into_iter()
                .map(|u| u.creator)
                .collect();

            let mut any_extended = false;
            for c in candidates {
                if self.maybe_extend(dbtx, next_round, c) {
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
    fn maybe_extend(&mut self, dbtx: &impl DbRead, round: Round, creator: PeerId) -> bool {
        if self.is_extended(round, creator) {
            return false;
        }

        let Some(signed) = dbtx.get(&self.units_table, &(round, creator)) else {
            return false;
        };

        if let Some(parent_round) = round.checked_sub(1) {
            let parents_fed = signed
                .unit
                .parents
                .iter()
                .all(|p| self.is_extended(parent_round, *p));
            if !parents_fed {
                return false;
            }
        }

        self.extended.insert((round, creator));

        true
    }

    /// Our own slot at `round-1` plus the lowest-`PeerId`-keyed
    /// `threshold - 1` other extended slots, or `None` if our own slot
    /// isn't extended yet or fewer than `threshold` slots are. Empty
    /// set for round 0. Filtering by `extended` (not mere presence)
    /// guarantees any unit we author is itself extendable on receivers.
    ///
    /// The self-parent is a creation-side rule only — receivers don't
    /// verify it. It chains our column: every unit of ours is an
    /// ancestor of all our later units, so one downstream reference of
    /// our column emits our whole backlog.
    fn parents_for(&self, round: Round) -> Option<BTreeSet<PeerId>> {
        let Some(parent_round) = round.checked_sub(1) else {
            return Some(BTreeSet::new());
        };

        if !self.is_extended(parent_round, self.id) {
            return None;
        }

        let t = self.n.threshold();

        let parents: BTreeSet<PeerId> = std::iter::once(self.id)
            .chain(
                self.extended
                    .range(
                        (parent_round, PeerId::from(0u8))..=(parent_round, PeerId::from(u8::MAX)),
                    )
                    .map(|(_, c)| *c)
                    .filter(|c| *c != self.id),
            )
            .take(t)
            .collect();

        (parents.len() == t).then_some(parents)
    }

    // --- db-read helpers over `units_table` ---

    pub(crate) fn round_units(&self, dbtx: &impl DbRead, round: Round) -> Vec<Unit<D>> {
        dbtx.range(&self.units_table, peer_range!(round), |it| {
            it.map(|(_, signed)| signed.unit).collect()
        })
    }

    pub(crate) fn highest_unit(
        &self,
        dbtx: &impl DbRead,
        creator: PeerId,
    ) -> Option<SignedUnit<D>> {
        dbtx.iter(&self.units_table, |it| {
            it.rev().find_map(|((_, c), s)| (c == creator).then_some(s))
        })
    }
}
