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
///   `handle_sig`).
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
            Message::Unit { unit, creator_sig } => self.handle_unit(sender, unit, creator_sig),
            Message::Sig {
                round,
                creator,
                signer,
                sig,
            } => self.handle_sig(sender, round, creator, signer, sig),
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

    /// Respond to a `Request` by unicasting back the body (with creator
    /// sig) and one `Sig` message per non-creator cosig we currently
    /// hold. The requester reassembles body + cosigs in their inbox.
    fn handle_request(&self, requester: PeerId, round: Round, creator: PeerId) {
        let Some(entry) = self.graph.entry(round, creator) else {
            return;
        };

        let creator_sig = *entry
            .sigs()
            .get(&creator)
            .expect("entry always carries the creator's sig");

        self.network.send(
            Recipient::Peer(requester),
            Message::Unit {
                unit: entry.unit().clone(),
                creator_sig,
            },
        );

        for (signer, sig) in entry.sigs() {
            if *signer == creator {
                continue;
            }
            self.network.send(
                Recipient::Peer(requester),
                Message::Sig {
                    round,
                    creator,
                    signer: *signer,
                    sig: *sig,
                },
            );
        }
    }

    /// Apply one inbound `Unit` message: verify the creator's sig,
    /// add the body to the graph (lax), splice in our own cosig if
    /// this is the first time we sign this slot, demand-pull any
    /// parents we don't yet hold *fed* from the immediate sender, and
    /// — only when our own cosig was newly added — broadcast a `Sig`
    /// so every other peer learns of our cosignature.
    ///
    /// `Unit` carries only the creator's sig now; cosigs flow on the
    /// lighter `Sig` channel (or piggyback on `Request` responses).
    ///
    /// Parent pulls run on every receive (fresh or duplicate). Re-issuing
    /// on duplicates makes the mechanism self-healing against dropped
    /// `Request` messages.
    fn handle_unit(&mut self, sender: PeerId, unit: Unit<D>, creator_sig: schnorr::Signature) {
        // Verify the body is signed by its claimed creator. Without
        // this, a Byzantine peer could fabricate a body at someone
        // else's slot, blocking the legitimate creator from inserting.
        if !self.keychain.verify(&unit, &creator_sig, unit.creator) {
            return;
        }

        // First-time-cosign detection: have we already signed this slot
        // locally? Skip if we're the creator (creator_sig is already
        // our sig — no separate cosig is meaningful).
        let already_signed_locally = self
            .graph
            .entry(unit.round, unit.creator)
            .is_some_and(|e| e.sigs().contains_key(&self.own_id));

        let mut sigs = BTreeMap::from([(unit.creator, creator_sig)]);

        let new_own_sig = if !already_signed_locally && unit.creator != self.own_id {
            let our_sig = self.keychain.sign(&unit);
            sigs.insert(self.own_id, our_sig);
            Some(our_sig)
        } else {
            None
        };

        // Snapshot before consuming `unit` for the parent-pull and
        // (potentially) the Sig fan-out below.
        let unit_round = unit.round;
        let unit_creator = unit.creator;
        let parents: Vec<PeerId> = unit.parents.iter().copied().collect();

        let _ = self.graph.insert_unit(unit, sigs, &self.keychain);

        if let Some(sig) = new_own_sig {
            self.network.send(
                Recipient::Everyone,
                Message::Sig {
                    round: unit_round,
                    creator: unit_creator,
                    signer: self.own_id,
                    sig,
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

    /// Apply one inbound `Sig` message. If we already hold the slot's
    /// body, verify and union the sig directly. If we don't, treat the
    /// `Sig` as a forward reference: drop it on the floor and unicast
    /// a `Request` to the signer, who definitely has the body — the
    /// response will arrive as a `Unit` carrying their full sig set
    /// (which includes `sig`), so we recover both body and sig in one
    /// roundtrip.
    fn handle_sig(
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
            session: self.graph.session(),
            round,
            creator: self.own_id,
            parents,
            data: self.data_provider.get_data().await,
        };

        let sigs = BTreeMap::from([(self.own_id, self.keychain.sign(&unit))]);

        // Crash barrier: persist the unit + our self-sig before
        // broadcasting. On restart we'd otherwise be free to build a
        // *different* unit at this slot from a fresh data_provider
        // draw, and peers who saw the original message would consider
        // us a forker.
        let entry = self
            .graph
            .insert_unit(unit, sigs, &self.keychain)
            .unwrap_or_else(|| panic!("newly built round-{round} unit must insert"));

        self.send_unit(Recipient::Everyone, &entry);
    }

    /// Ship an entry's body (with creator sig only) — used by the
    /// creator's broadcast on creation and the anti-entropy push.
    /// Cosig fan-out is the `Sig` channel's job, and the request-response
    /// path emits its own `Unit` + per-cosig `Sig` directly.
    fn send_unit(&self, recipient: Recipient, entry: &Entry<D>) {
        let creator = entry.unit().creator;
        let creator_sig = *entry
            .sigs()
            .get(&creator)
            .expect("entry always carries the creator's sig");
        self.network.send(
            recipient,
            Message::Unit {
                unit: entry.unit().clone(),
                creator_sig,
            },
        );
    }
}
