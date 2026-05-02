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
            Message::Unit { unit, sigs } => self.handle_unit(sender, unit, sigs),
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
            self.send_entry(Recipient::Everyone, entry);
        }
    }

    /// Respond to a `Request` for `(round, creator)` by unicasting our
    /// entry at that slot — body plus all sigs we currently hold — back
    /// to the requester.
    fn handle_request(&self, requester: PeerId, round: Round, creator: PeerId) {
        if let Some(entry) = self.graph.entry(round, creator) {
            self.send_entry(Recipient::Peer(requester), entry);
        }
    }

    /// Apply one inbound `Unit` message: validate the bundle, splice in
    /// our own co-sig if this is the first time we sign this slot,
    /// hand the whole thing to the graph, demand-pull any of the unit's
    /// parents we don't yet hold *fed* from the immediate sender, and
    /// — only when our own cosig was newly added — broadcast a `Sig`
    /// message so every other peer learns of our cosignature.
    ///
    /// We don't rebroadcast the body. Bodies flow only on creator
    /// broadcast at unit-creation time, on creator's anti-entropy push
    /// of own slot, and on `Request` response. Cosig fan-out moves
    /// through the lighter `Sig` channel.
    ///
    /// Parent pulls run on every receive (fresh or duplicate). Re-issuing
    /// on duplicates makes the mechanism self-healing against dropped
    /// `Request` messages.
    fn handle_unit(
        &mut self,
        sender: PeerId,
        unit: Unit<D>,
        mut sigs: BTreeMap<PeerId, schnorr::Signature>,
    ) {
        // Creator's sig must be in the bundle: it binds the body to its
        // claimed author. Without this check, a Byzantine peer could
        // send a fabricated body at someone else's slot signed only with
        // their own key, blocking the legitimate creator from inserting.
        if !sigs.contains_key(&unit.creator) {
            return;
        }

        // Cap verify cost: a malicious peer can't make us check more
        // sigs than a fully-confirmed unit would carry.
        if sigs.len() > self.graph.threshold() {
            return;
        }

        // Every carried sig must verify against the unit under its
        // claimed signer.
        if !sigs
            .iter()
            .all(|(signer, sig)| self.keychain.verify(&unit, sig, *signer))
        {
            return;
        }

        // First-time-cosign detection: have we already signed this slot
        // locally? Either via a previous insert that stored our sig, or
        // via the incoming bundle already carrying it. If not, splice
        // our sig in *and* remember to broadcast it after insert.
        let already_signed_locally = self
            .graph
            .entry(unit.round, unit.creator)
            .is_some_and(|e| e.sigs().contains_key(&self.own_id));

        let new_own_sig = if !already_signed_locally
            && !sigs.contains_key(&self.own_id)
            && sigs.len() < self.graph.threshold()
        {
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

        self.send_entry(Recipient::Everyone, &entry);
    }

    /// Single helper for shipping an entry on the wire — used by the
    /// anti-entropy push, the request response, and the cosign
    /// rebroadcast.
    fn send_entry(&self, recipient: Recipient, entry: &Entry<D>) {
        self.network.send(
            recipient,
            Message::Unit {
                unit: entry.unit().clone(),
                sigs: entry.sigs().clone(),
            },
        );
    }
}
