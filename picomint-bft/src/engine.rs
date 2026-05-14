use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use async_channel::Sender;
use picomint_core::config::BFT_UNIT_BYTE_LIMIT;
use picomint_core::secp256k1::schnorr;
use picomint_core::{NumPeers, PeerId, peer_range};
use picomint_encoding::Encodable;
use picomint_redb::{Database, DbRead, Table, TableDef, WriteTx};
use tokio::time::{Instant, sleep_until};
use tracing::warn;

use crate::data::DataProvider;
use crate::keychain::Keychain;
use crate::network::{DynNetwork, Message, Recipient};
use crate::unit::{Cosig, Round, Unit, UnitData};

/// Periodic own-slot push interval. Pull is demand-driven, not periodic.
const ANTI_ENTROPY_INTERVAL: Duration = Duration::from_secs(1);

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
    own_id: PeerId,
    session: u64,
    pub(crate) n: NumPeers,
    db: Database,
    keychain: Keychain,
    network: DynNetwork<D>,
    data_provider: P,
    unit_delay: Box<dyn Fn(Round) -> Duration + Send + 'static>,
    pub(crate) ordered_tx: Sender<(Round, PeerId, D)>,

    /// Daemon-declared units table (`(Round, PeerId) => Unit<D>`).
    /// Bft only reads/writes it.
    pub(crate) units_table: TableDef<(Round, PeerId), Unit<D>>,
    /// Daemon-declared cosigs table
    /// (`(Round, PeerId, PeerId) => Cosig`). The creator's own
    /// signature lives here at `(round, creator, creator)`.
    pub(crate) cosigs_table: TableDef<(Round, PeerId, PeerId), Cosig>,

    /// Slots whose row count in `cosigs_table` meets threshold *and*
    /// every parent slot is itself in this set. Rebuilt from disk on
    /// startup; never persisted.
    pub(crate) extended: BTreeSet<(Round, PeerId)>,
    /// Slots whose payload has been sent through `ordered_tx`.
    /// Prevents re-emission across batches and within one BFS.
    pub(crate) emitted: BTreeSet<(Round, PeerId)>,
    /// Extender cursor: the next leader round to attempt deciding.
    pub(crate) next_decide_round: Round,
}

impl<P, D> Engine<P, D>
where
    D: UnitData,
    P: DataProvider<D>,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new<TU, TC>(
        own_id: PeerId,
        session: u64,
        n: NumPeers,
        db: Database,
        keychain: Keychain,
        network: DynNetwork<D>,
        data_provider: P,
        unit_delay: Box<dyn Fn(Round) -> Duration + Send + 'static>,
        ordered_tx: Sender<(Round, PeerId, D)>,
        units_table: TU,
        cosigs_table: TC,
    ) -> Self
    where
        TU: Table<Key = (Round, PeerId), Value = Unit<D>>,
        TC: Table<Key = (Round, PeerId, PeerId), Value = Cosig>,
    {
        Self {
            own_id,
            session,
            n,
            db,
            keychain,
            network,
            data_provider,
            unit_delay,
            ordered_tx,
            units_table: TableDef::from(units_table),
            cosigs_table: TableDef::from(cosigs_table),
            extended: BTreeSet::new(),
            emitted: BTreeSet::new(),
            next_decide_round: 0,
        }
    }

    pub async fn run(mut self) {
        self.replay().await;

        let mut next_create_at = Instant::now();
        let mut next_anti_entropy_at = Instant::now();

        loop {
            tokio::select! {
                maybe_msg = self.network.receive() => {
                    let Some((sender, msg)) = maybe_msg else { return };

                    if let Err(err) = self.handle_message(sender, msg).await {
                        warn!(%sender, err = %format_args!("{err:#}"), "rejected bft message");
                    }
                }

                _ = sleep_until(next_create_at) => {
                    self.try_create_unit().await;

                    next_create_at = Instant::now()
                        + (self.unit_delay)(self.next_create_round());
                }

                _ = sleep_until(next_anti_entropy_at) => {
                    self.broadcast_anti_entropy();

                    next_anti_entropy_at = Instant::now() + ANTI_ENTROPY_INTERVAL;
                }
            }
        }
    }

    /// Rebuild the in-memory `extended` / `emitted` / `next_decide_round`
    /// from persisted `BFT_UNITS` + `BFT_COSIGS`, and re-emit every
    /// committed item through `ordered_tx`.
    ///
    /// Correctness rests on determinism: `try_extend` is a fixpoint over
    /// the parent-extended predicate, and the extender's vote tally +
    /// `bfs_batch` are both deterministic over the final unit/cosig set.
    /// So calling `try_extend(0, c)` for every round-zero creator (the
    /// cascade root) and then `run_extender` once produces the same
    /// `extended` set and the same channel emission sequence as the
    /// live unit-by-unit growth did before the restart.
    async fn replay(&mut self) {
        let dbtx = self.db.begin_read();
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
    /// not rolled back on Err — only the persistent `BFT_UNITS` /
    /// `BFT_COSIGS` writes are. The mutators only run after the dbtx
    /// writes succeed via `?`.
    async fn handle_message(&mut self, sender: PeerId, msg: Message<D>) -> Result<()> {
        match msg {
            Message::Unit { unit, sig } => {
                let dbtx = self.db.begin_write();

                self.handle_unit(&dbtx, sender, &unit, sig)?;
                self.try_extend(&dbtx, unit.round, unit.creator);
                self.run_extender(&dbtx).await;

                dbtx.commit();
            }
            Message::Cosig {
                round,
                creator,
                signer,
                cosig,
            } => {
                let dbtx = self.db.begin_write();

                self.record_cosig(&dbtx, round, creator, signer, cosig)?;
                self.try_extend(&dbtx, round, creator);
                self.run_extender(&dbtx).await;

                dbtx.commit();
            }
            Message::SignedUnit { unit, cosigs } => {
                let dbtx = self.db.begin_write();

                self.handle_signed_unit(&dbtx, sender, &unit, cosigs)?;
                self.try_extend(&dbtx, unit.round, unit.creator);
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

        let Some(unit) = self.highest_unit(&dbtx, self.own_id) else {
            return;
        };

        let sig = *self
            .cosigs(&dbtx, unit.round, self.own_id)
            .get(&self.own_id)
            .expect("we always have our own signature for our own unit");

        self.send_unit(Recipient::Everyone, &unit, &sig);
    }

    /// Reply with `SignedUnit` only when the slot is locally confirmed.
    /// A sub-threshold bundle would be unsafe — the receiver overwrites
    /// its entry on the strength of the threshold proof.
    fn handle_request(&self, dbtx: &impl DbRead, requester: PeerId, round: Round, creator: PeerId) {
        let Some(unit) = dbtx.get(&self.units_table, &(round, creator)) else {
            return;
        };

        if !self.is_confirmed(dbtx, round, creator) {
            return;
        }

        let cosigs = self.cosigs(dbtx, round, creator);

        self.network.send(
            Recipient::Peer(requester),
            Message::SignedUnit { unit, cosigs },
        );
    }

    /// Rebroadcast our existing cosig (retry for dropped Cosig msgs),
    /// demand-pull any not-yet-extended parents (retry for dropped
    /// Request msgs), then attempt fresh install + own cosign. A
    /// duplicate-slot insert errors via `?` and rolls back; the
    /// network sends above are fire-and-forget and persist.
    fn handle_unit(
        &mut self,
        dbtx: &WriteTx,
        sender: PeerId,
        unit: &Unit<D>,
        sig: schnorr::Signature,
    ) -> Result<()> {
        if let Some(cosig) = dbtx.get(&self.cosigs_table, &(unit.round, unit.creator, self.own_id))
        {
            self.network.send(
                Recipient::Everyone,
                Message::Cosig {
                    round: unit.round,
                    creator: unit.creator,
                    signer: self.own_id,
                    cosig: cosig.0,
                },
            );
        }

        if let Some(parent_round) = unit.round.checked_sub(1) {
            for parent_creator in unit.parents.iter().copied() {
                if !self.is_extended(parent_round, parent_creator) {
                    self.network.send(
                        Recipient::Peer(sender),
                        Message::Request {
                            round: parent_round,
                            creator: parent_creator,
                        },
                    );
                }
            }
        }

        self.insert_unit(dbtx, unit, sig)?;

        let cosig = self.keychain.sign(self.session, unit);

        self.record_cosig(dbtx, unit.round, unit.creator, self.own_id, cosig)
            .expect("own cosig over freshly inserted body must succeed");

        self.network.send(
            Recipient::Everyone,
            Message::Cosig {
                round: unit.round,
                creator: unit.creator,
                signer: self.own_id,
                cosig,
            },
        );

        Ok(())
    }

    /// Atomically install/overwrite from the threshold proof, then
    /// demand-pull any not-yet-extended parents from the sender. No
    /// rebroadcast, no cosig fan-out — this is a pull-driven event.
    fn handle_signed_unit(
        &mut self,
        dbtx: &WriteTx,
        sender: PeerId,
        unit: &Unit<D>,
        cosigs: BTreeMap<PeerId, schnorr::Signature>,
    ) -> Result<()> {
        self.insert_signed_unit(dbtx, unit, cosigs)?;

        if let Some(parent_round) = unit.round.checked_sub(1) {
            for parent_creator in unit.parents.iter().copied() {
                if !self.is_extended(parent_round, parent_creator) {
                    self.network.send(
                        Recipient::Peer(sender),
                        Message::Request {
                            round: parent_round,
                            creator: parent_creator,
                        },
                    );
                }
            }
        }

        Ok(())
    }

    /// Validate and install a fresh `(round, creator)` slot — body in
    /// `BFT_UNITS`, creator's sig in `BFT_COSIGS` at `(_, _, creator)`.
    /// Caller must check absence beforehand: this is a one-shot install.
    fn insert_unit(&self, dbtx: &WriteTx, unit: &Unit<D>, sig: schnorr::Signature) -> Result<()> {
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
            self.keychain.verify(self.session, unit, &sig, unit.creator),
            "invalid creator signature",
        );

        ensure!(
            dbtx.insert(&self.units_table, &(unit.round, unit.creator), unit)
                .is_none(),
            "unit slot already occupied",
        );

        dbtx.insert(
            &self.cosigs_table,
            &(unit.round, unit.creator, unit.creator),
            &Cosig(sig),
        );

        Ok(())
    }

    /// Install (or overwrite) a slot from a threshold-proven bundle.
    /// A valid `SignedUnit` proves canonical body — quorum math forbids
    /// two distinct bodies reaching threshold — so overwrite is safe.
    /// Stale cosigs over a divergent body are cleared as a side effect.
    fn insert_signed_unit(
        &self,
        dbtx: &WriteTx,
        unit: &Unit<D>,
        cosigs: BTreeMap<PeerId, schnorr::Signature>,
    ) -> Result<()> {
        ensure!(
            unit.data.consensus_encode_to_vec().len() <= BFT_UNIT_BYTE_LIMIT,
            "unit body exceeds size limit",
        );

        ensure!(cosigs.len() == self.n.threshold(), "wrong number of cosigs");

        ensure!(
            cosigs.contains_key(&unit.creator),
            "creator signature missing",
        );

        for (signer, c) in &cosigs {
            ensure!(
                self.keychain.verify(self.session, unit, c, *signer),
                "invalid cosig signature",
            );
        }

        dbtx.insert(&self.units_table, &(unit.round, unit.creator), unit);

        // Overwrite the slot's full cosig set; signers absent from the
        // bundle have any stale sig (over a divergent body) removed.
        for signer in self.n.peer_ids() {
            if let Some(c) = cosigs.get(&signer) {
                dbtx.insert(
                    &self.cosigs_table,
                    &(unit.round, unit.creator, signer),
                    &Cosig(*c),
                );
            } else {
                dbtx.remove(&self.cosigs_table, &(unit.round, unit.creator, signer));
            }
        }

        Ok(())
    }

    /// Verify and merge `signer`'s cosig over the body we hold for
    /// `(round, creator)`. Errors on missing body, already-confirmed
    /// slot, invalid sig, or duplicate; the per-message dbtx rollback
    /// handles cleanup.
    ///
    /// Verifying against the *locally-held* body is the consistent-
    /// broadcast check: a forker's cosigs over a different body don't
    /// verify against ours, so neither side reaches threshold.
    fn record_cosig(
        &self,
        dbtx: &WriteTx,
        round: Round,
        creator: PeerId,
        signer: PeerId,
        sig: schnorr::Signature,
    ) -> Result<()> {
        let unit: Unit<D> = dbtx
            .get(&self.units_table, &(round, creator))
            .context("no unit for signature")?;

        ensure!(
            !self.is_confirmed(dbtx, round, creator),
            "unit already confirmed",
        );

        ensure!(
            self.keychain.verify(self.session, &unit, &sig, signer),
            "invalid cosig signature",
        );

        ensure!(
            dbtx.insert(&self.cosigs_table, &(round, creator, signer), &Cosig(sig))
                .is_none(),
            "duplicate cosig",
        );

        Ok(())
    }

    /// Derive the next round to attempt creation at from our highest
    /// own-slot. After wipe-and-recover, peers may have refilled our
    /// slot via anti-entropy; `highest_unit` accounts for that, so
    /// we resume at `highest + 1` rather than re-forking it.
    fn next_create_round(&self) -> Round {
        let dbtx = self.db.begin_read();
        self.highest_unit(&dbtx, self.own_id)
            .map_or(0, |u| u.round + 1)
    }

    async fn try_create_unit(&mut self) {
        let round = self.next_create_round();

        let Some(parents) = self.parents_for(round) else {
            return;
        };

        let data: Vec<D> = self.data_provider.get_data().await;

        let unit = Unit {
            round,
            creator: self.own_id,
            parents,
            data,
        };

        let sig = self.keychain.sign(self.session, &unit);

        let dbtx = self.db.begin_write();

        // Crash barrier: persist before broadcasting, otherwise a
        // restart would let us build a *different* unit at this slot
        // from a fresh data_provider draw — peers that saw the
        // original would consider us a forker.
        self.insert_unit(&dbtx, &unit, sig)
            .expect("newly built unit must insert");

        self.try_extend(&dbtx, round, self.own_id);

        self.run_extender(&dbtx).await;

        dbtx.commit();

        self.send_unit(Recipient::Everyone, &unit, &sig);
    }

    fn send_unit(&self, recipient: Recipient, unit: &Unit<D>, sig: &schnorr::Signature) {
        self.network.send(
            recipient,
            Message::Unit {
                unit: unit.clone(),
                sig: *sig,
            },
        );
    }

    // --- in-memory extension state ---

    /// Confirmed *and* every parent slot is extended.
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

        let Some(unit) = dbtx.get(&self.units_table, &(round, creator)) else {
            return false;
        };

        if !self.is_confirmed(dbtx, round, creator) {
            return false;
        }

        if let Some(parent_round) = round.checked_sub(1) {
            let parents_fed = unit
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

    /// Lowest-`PeerId`-keyed `threshold` extended slots at `round-1`,
    /// or `None` if fewer than `threshold` slots there are extended.
    /// Empty set for round 0. Filtering by `extended` (not `confirmed`)
    /// guarantees any unit we author is itself extendable on receivers.
    fn parents_for(&self, round: Round) -> Option<BTreeSet<PeerId>> {
        let Some(parent_round) = round.checked_sub(1) else {
            return Some(BTreeSet::new());
        };

        let t = self.n.threshold();

        let parents: BTreeSet<PeerId> = self
            .extended
            .range((parent_round, PeerId::from(0u8))..=(parent_round, PeerId::from(u8::MAX)))
            .take(t)
            .map(|(_, c)| *c)
            .collect();

        (parents.len() == t).then_some(parents)
    }

    // --- db-read helpers over `units_table` / `cosigs_table` ---

    /// All signatures collected for `(round, creator)`, including the
    /// creator's own signature at `signer == creator`.
    pub(crate) fn cosigs(
        &self,
        dbtx: &impl DbRead,
        round: Round,
        creator: PeerId,
    ) -> BTreeMap<PeerId, schnorr::Signature> {
        dbtx.range(&self.cosigs_table, peer_range!(round, creator), |it| {
            it.map(|((_, _, signer), Cosig(sig))| (signer, sig))
                .collect()
        })
    }

    fn sig_count(&self, dbtx: &impl DbRead, round: Round, creator: PeerId) -> usize {
        dbtx.range(&self.cosigs_table, peer_range!(round, creator), |it| {
            it.count()
        })
    }

    /// At least `threshold` signatures collected. Does *not* imply
    /// ancestors are ready.
    pub(crate) fn is_confirmed(&self, dbtx: &impl DbRead, round: Round, creator: PeerId) -> bool {
        self.sig_count(dbtx, round, creator) >= self.n.threshold()
    }

    pub(crate) fn round_units(&self, dbtx: &impl DbRead, round: Round) -> Vec<Unit<D>> {
        dbtx.range(&self.units_table, peer_range!(round), |it| {
            it.map(|(_, u)| u).collect()
        })
    }

    pub(crate) fn highest_unit(&self, dbtx: &impl DbRead, creator: PeerId) -> Option<Unit<D>> {
        dbtx.iter(&self.units_table, |it| {
            it.rev().find_map(|((_, c), u)| (c == creator).then_some(u))
        })
    }
}
