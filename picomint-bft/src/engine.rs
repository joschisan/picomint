use std::collections::BTreeMap;
use std::time::Duration;

use picomint_core::PeerId;
use picomint_core::secp256k1::schnorr;
use tokio::time::{Instant, sleep_until};

use crate::data::DataProvider;
use crate::graph::{Entry, Graph};
use crate::keychain::Keychain;
use crate::network::{DynNetwork, Message, Recipient};
use crate::unit::{Round, Unit, UnitData};

/// How often each peer fires the periodic anti-entropy push: send our
/// own highest entry (with the sigs we currently hold) to everyone.
/// Each peer is canonical for its own column — pushing only the own
/// slot gives laggards a reentry point from which they walk back via
/// demand-pull on missing parents.
///
/// Pull is demand-driven, not periodic: on every received unit, we
/// unicast a `Request` to the sender for any of that unit's parents
/// we don't yet hold *fed* locally. Re-issued on every receive (fresh
/// or duplicate), so a dropped `Request` is retried the next time the
/// pushing peer ships us the same child.
const ANTI_ENTROPY_INTERVAL: Duration = Duration::from_secs(1);

/// Drive a single peer's growth indefinitely.
///
/// Three concurrent arms drive the loop:
///
/// - **Inbound messages** are verified and folded into the local
///   `Graph`. The graph internally persists every mutation through
///   `backup` and feeds confirmed units into its extender, which emits
///   ordered `(round, creator, datum)` triples on `ordered_tx`.
/// - **Anti-entropy** every `ANTI_ENTROPY_INTERVAL`: push our own
///   highest entry to everyone. Other peers' columns flow only on
///   explicit `Request` (issued reactively from `handle_unit` and
///   `handle_cosig`).
/// - **Unit creation** is rate-limited by `unit_delay`, a closure
///   `Round → Duration` invoked with the round we're *about* to attempt
///   to create. The first deadline fires immediately at startup, so
///   `unit_delay(0)` typically returns 0; the daemon's caller threads
///   in an exponential-slowdown formula so a session that fails to
///   terminate (e.g. under attack) can't grow rounds without bound.
///
/// The engine has no internal stopping condition — it runs until the
/// caller drops the task (or until every other peer has done so, which
/// closes the network).
pub async fn run<D: UnitData, P: DataProvider<D>>(
    own_id: PeerId,
    graph: Graph<D>,
    keychain: Keychain,
    network: DynNetwork<Message<D>>,
    data_provider: P,
    unit_delay: Box<dyn Fn(Round) -> Duration + Send + 'static>,
) {
    Engine {
        own_id,
        graph,
        keychain,
        network,
        data_provider,
        unit_delay,
    }
    .run()
    .await;
}

/// All long-lived state for one peer's engine. Kept private — `pub
/// async fn run` is the only way in.
struct Engine<D: UnitData, P: DataProvider<D>> {
    own_id: PeerId,
    graph: Graph<D>,
    keychain: Keychain,
    network: DynNetwork<Message<D>>,
    data_provider: P,
    unit_delay: Box<dyn Fn(Round) -> Duration + Send + 'static>,
}

impl<D: UnitData, P: DataProvider<D>> Engine<D, P> {
    async fn run(mut self) {
        let mut next_create_at = Instant::now();
        let mut next_anti_entropy_at = Instant::now();

        loop {
            tokio::select! {
                maybe_msg = self.network.receive() => {
                    let Some((sender, msg)) = maybe_msg else { return };

                    self.handle_message(sender, msg);
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

    fn handle_message(&mut self, sender: PeerId, msg: Message<D>) {
        match msg {
            Message::Unit { unit, sig } => self.handle_unit(sender, unit, sig),
            Message::Cosig {
                round,
                creator,
                signer,
                sig,
            } => self.handle_cosig(sender, round, creator, signer, sig),
            Message::SignedUnit { unit, sig, cosigs } => {
                self.handle_signed_unit(sender, unit, sig, cosigs);
            }
            Message::Request { round, creator } => {
                self.handle_request(sender, round, creator);
            }
        }
    }

    /// Push-only anti-entropy, narrowed to *own* slot only. Each peer
    /// is canonical for its own column of the DAG: it owns the body and
    /// accumulates sigs locally as they arrive (via `Sig` broadcasts
    /// and `Request` responses). Pushing only the own highest slot
    /// (with whatever sigs we hold) gives laggards a reentry point —
    /// from any push they walk back via demand-pull on missing parents.
    /// Other peers' slots flow only on explicit `Request`, so no peer
    /// has to maintain a redundant view of the federation's whole DAG
    /// in steady-state push traffic.
    fn broadcast_anti_entropy(&self) {
        if let Some(entry) = self.graph.highest_entry(self.own_id) {
            self.send_unit(Recipient::Everyone, entry);
        }
    }

    /// Respond to a `Request` by unicasting back a `SignedUnit` —
    /// body + creator sig + exactly `2f` cosigs — but *only* if the
    /// slot is locally confirmed (≥ threshold sigs). If the slot is
    /// missing or below threshold we send nothing; the requester will
    /// retry on its next anti-entropy cycle.
    ///
    /// Limiting responses to confirmed slots is what makes `SignedUnit`
    /// load-bearing: a receiver that overwrites its local entry is
    /// trusting the threshold proof in the bundle, so we must never
    /// ship a sub-threshold bundle.
    fn handle_request(&self, requester: PeerId, round: Round, creator: PeerId) {
        let Some(entry) = self.graph.entry(round, creator) else {
            return;
        };

        if !entry.is_confirmed(self.graph.threshold()) {
            return;
        }

        // `record_cosig` stops accepting cosigs at threshold, so
        // `entry.cosigs` already holds exactly `2f` cosigs by
        // invariant — no trim needed.
        self.network.send(
            Recipient::Peer(requester),
            Message::SignedUnit {
                unit: entry.unit().clone(),
                sig: *entry.sig(),
                cosigs: entry.cosigs().clone(),
            },
        );
    }

    /// Apply one inbound `Unit` message: verify the creator's sig,
    /// add the body to the graph (lax), record our own cosig if this
    /// is the first time we sign this slot, demand-pull any parents
    /// we don't yet hold *fed* from the immediate sender, and — only
    /// when our own cosig was newly added — broadcast a `Cosig` so
    /// every other peer learns of our cosignature.
    ///
    /// `Unit` carries only the creator's sig; cosigs flow on the
    /// lighter `Cosig` channel (or piggyback on `Request` responses).
    ///
    /// Parent pulls run on every receive (fresh or duplicate). Re-issuing
    /// on duplicates makes the mechanism self-healing against dropped
    /// `Request` messages.
    fn handle_unit(&mut self, sender: PeerId, unit: Unit<D>, sig: schnorr::Signature) {
        // Verify the body is signed by its claimed creator under the
        // current session. Without this, a Byzantine peer could
        // fabricate a body at someone else's slot, blocking the
        // legitimate creator from inserting; and a stale unit from an
        // earlier session would be hashed under a different `(session,
        // unit)` tuple and fail to verify.
        if !self
            .keychain
            .verify(self.graph.session(), &unit, &sig, unit.creator)
        {
            return;
        }

        let unit_round = unit.round;
        let unit_creator = unit.creator;
        let parents: Vec<PeerId> = unit.parents.iter().copied().collect();

        // First-time-cosign detection. Skip if we're the creator (our
        // sig is already in the entry's `sig` field — no separate cosig
        // is meaningful).
        let already_signed_locally = self
            .graph
            .entry(unit_round, unit_creator)
            .is_some_and(|e| e.cosigs().contains_key(&self.own_id));

        let new_own_cosig = if !already_signed_locally && unit_creator != self.own_id {
            Some(self.keychain.sign(self.graph.session(), &unit))
        } else {
            None
        };

        let _ = self
            .graph
            .insert_unit(unit, sig, BTreeMap::new(), &self.keychain);

        if let Some(cosig) = new_own_cosig {
            self.graph
                .record_cosig(unit_round, unit_creator, self.own_id, cosig, &self.keychain);
            self.network.send(
                Recipient::Everyone,
                Message::Cosig {
                    round: unit_round,
                    creator: unit_creator,
                    signer: self.own_id,
                    sig: cosig,
                },
            );
        }

        // Demand-pull every parent slot we don't yet hold *fed*. Covers
        // three cases at once: (a) parent body missing, (b) parent
        // present but below sig threshold (the sender's response unions
        // any sigs we're missing), (c) parent confirmed but ancestrally
        // unfed (re-receiving the body re-fires this same parent-pull
        // logic recursively, retrying the deeper walk-back). Once the
        // parent is fed, the request stops firing.
        if let Some(parent_round) = unit_round.checked_sub(1) {
            for parent_creator in parents {
                if !self.graph.is_fed(parent_round, parent_creator) {
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
    }

    /// Apply one inbound `SignedUnit` message — the threshold-proven
    /// body + cosig bundle. The graph atomically installs (or
    /// overwrites) the slot, then we demand-pull any of the unit's
    /// parents we don't yet hold *fed* from the immediate sender. No
    /// rebroadcast, no Cosig fan-out — receiving a SignedUnit is a
    /// pull-driven event, not a fresh consensus contribution.
    fn handle_signed_unit(
        &mut self,
        sender: PeerId,
        unit: Unit<D>,
        sig: schnorr::Signature,
        cosigs: BTreeMap<PeerId, schnorr::Signature>,
    ) {
        let parent_round = unit.round.checked_sub(1);
        let parents: Vec<PeerId> = unit.parents.iter().copied().collect();

        if !self
            .graph
            .insert_signed_unit(unit, sig, cosigs, &self.keychain)
        {
            return;
        }

        if let Some(parent_round) = parent_round {
            for parent_creator in parents {
                if !self.graph.is_fed(parent_round, parent_creator) {
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
    }

    /// Apply one inbound `Cosig` message. If we already hold the slot's
    /// body, verify and union the cosig directly. If we don't, treat
    /// the `Cosig` as a forward reference: drop the cosig and unicast
    /// a `Request` to the signer (who must hold the body since they
    /// signed it). The response arrives as a `SignedUnit` if the
    /// signer has accumulated threshold cosigs locally, recovering
    /// both body and the dropped cosig in one round-trip.
    fn handle_cosig(
        &mut self,
        sender: PeerId,
        round: Round,
        creator: PeerId,
        signer: PeerId,
        sig: schnorr::Signature,
    ) {
        if !self
            .graph
            .record_cosig(round, creator, signer, sig, &self.keychain)
        {
            self.network
                .send(Recipient::Peer(sender), Message::Request { round, creator });
        }
    }

    /// Round we're about to attempt to create at, derived from the
    /// highest own-slot we currently hold. After a wipe-and-restore,
    /// peers can fill our slot via anti-entropy before this fires —
    /// `highest_entry` already accounts for that, so we naturally
    /// resume at `highest + 1` rather than risking a fork by rebuilding
    /// a slot peers have already endorsed.
    fn next_create_round(&self) -> Round {
        self.graph
            .highest_entry(self.own_id)
            .map_or(0, |e| e.unit().round + 1)
    }

    async fn try_create_unit(&mut self) {
        let round = self.next_create_round();

        let Some(parents) = self.graph.parents_for(round) else {
            return;
        };

        let unit = Unit {
            round,
            creator: self.own_id,
            parents,
            data: self.data_provider.get_data().await,
        };

        let sig = self.keychain.sign(self.graph.session(), &unit);

        // Crash barrier: persist the unit + our self-sig before
        // broadcasting. On restart we'd otherwise be free to build a
        // *different* unit at this slot from a fresh data_provider
        // draw, and peers who saw the original message would consider
        // us a forker.
        let entry = self
            .graph
            .insert_unit(unit, sig, BTreeMap::new(), &self.keychain)
            .unwrap_or_else(|| panic!("newly built round-{round} unit must insert"));

        self.send_unit(Recipient::Everyone, &entry);
    }

    /// Ship an entry's body (with creator sig only) — used by the
    /// creator's broadcast on creation and the anti-entropy push.
    /// Cosig fan-out is the `Cosig` channel's job, and the
    /// request-response path emits its own `Unit` + per-cosig `Cosig`
    /// directly.
    fn send_unit(&self, recipient: Recipient, entry: &Entry<D>) {
        self.network.send(
            recipient,
            Message::Unit {
                unit: entry.unit().clone(),
                sig: *entry.sig(),
            },
        );
    }
}
