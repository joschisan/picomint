use std::collections::BTreeMap;
use std::time::Duration;

use picomint_core::PeerId;
use picomint_core::secp256k1::schnorr;
use tokio::time::{Instant, sleep_until};

use crate::data::DataProvider;
use crate::graph::{Entry, Graph, InsertOutcome};
use crate::keychain::Keychain;
use crate::network::{DynNetwork, Message, Recipient};
use crate::unit::{Round, Unit, UnitData};

/// How often each peer fires the periodic anti-entropy:
///
/// - **push**: for every peer in the federation, send our highest known
///   entry (with the sigs we hold) to everyone. Refills sig deficits at
///   slots receivers already hold and seeds higher rounds at laggards.
/// - **pull**: for every peer, send a `Request` for the lowest round
///   where we don't yet have that peer's slot confirmed locally. Pulls
///   in missing units one round at a time per peer.
const ANTI_ENTROPY_INTERVAL: Duration = Duration::from_millis(50);

/// Drive a single peer's growth indefinitely.
///
/// Three concurrent arms drive the loop:
///
/// - **Inbound messages** are verified and folded into the local
///   `Graph`. The graph internally persists every mutation through
///   `backup` and feeds confirmed units into its extender, which emits
///   ordered `(round, creator, datum)` triples on `ordered_tx`.
/// - **Anti-entropy** every `ANTI_ENTROPY_INTERVAL`: push the highest
///   known entry per peer to everyone, and pull (one `Request` per peer)
///   for the lowest round we don't yet have confirmed.
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
    let next_round = graph
        .highest_entry(own_id)
        .map_or(0, |e| e.unit().round + 1);

    Engine {
        own_id,
        graph,
        keychain,
        network,
        data_provider,
        unit_delay,
        next_round,
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
    /// The next round we'll attempt to create a unit at. Starts at 0
    /// (no genesis pre-population — round 0 is created and disseminated
    /// like any other round) or one past our highest restored own-slot.
    /// Bumped past every round we either skip or successfully build.
    next_round: Round,
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
                        + (self.unit_delay)(self.next_round);
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
            Message::Unit { unit, sigs } => self.handle_unit(unit, sigs),
            Message::Request { round, creator } => {
                self.handle_request(sender, round, creator);
            }
        }
    }

    /// Per-cycle anti-entropy. Two legs, both bounded at one message per
    /// peer in the federation:
    ///
    /// - **Push**: for every peer, send our highest known entry for that
    ///   peer (with the sigs we currently hold) to everyone. Receivers
    ///   union sigs into their own view; if their copy was below
    ///   threshold it may now confirm. New entries at higher rounds are
    ///   accepted by strict insert iff their parents are already
    ///   confirmed locally; otherwise dropped, to be re-delivered after
    ///   pull has caught the receiver up.
    ///
    /// - **Pull**: for every peer, send a `Request` for the lowest round
    ///   where we don't yet hold that peer's slot confirmed. Recipients
    ///   that hold the slot reply with their entry. Idempotent — the
    ///   same round is re-requested next cycle until it confirms.
    fn broadcast_anti_entropy(&self) {
        for creator in self.graph.peer_ids() {
            if let Some(entry) = self.graph.highest_entry(creator) {
                self.send_entry(Recipient::Everyone, entry);
            }

            let round = self.graph.lowest_unconfirmed_round(creator);

            self.network
                .send(Recipient::Everyone, Message::Request { round, creator });
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

    /// Apply one inbound `Unit` message:
    ///
    /// - Validate (creator-sig present, sigs cap, every carried sig
    ///   verifies).
    /// - Insert (strict — drops anything whose parents aren't confirmed
    ///   locally; the periodic anti-entropy will refill the
    ///   prerequisites and the unit will arrive again on a later cycle).
    /// - Merge each carried sig; the graph internally persists every
    ///   mutation and feeds the unit to the extender if this sig pushes
    ///   the slot across the threshold.
    /// - If we haven't co-signed yet, sign and rebroadcast so peers
    ///   union our contribution.
    fn handle_unit(&mut self, unit: Unit<D>, sigs: BTreeMap<PeerId, schnorr::Signature>) {
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

        // Insert the body. `Duplicate` is fine — we already had it.
        // `WrongSession`, `OversizedData`, `InvalidParents`,
        // `MissingParents` all drop; `MissingParents` is the expected
        // steady-state miss when anti-entropy hasn't yet caught us up to
        // the broadcasting peer.
        match self.graph.insert_unit(unit.clone()) {
            InsertOutcome::Accepted | InsertOutcome::Duplicate => {}
            _ => return,
        }

        for (signer, sig) in &sigs {
            self.graph
                .record_sig(unit.round, unit.creator, *signer, *sig, &self.keychain);
        }

        // If our own sig isn't yet at this slot — and the slot hasn't
        // already confirmed — sign now and rebroadcast.
        let threshold = self.graph.threshold();
        let needs_cosign = self
            .graph
            .entry(unit.round, unit.creator)
            .is_some_and(|e| !e.sigs().contains_key(&self.own_id) && !e.is_confirmed(threshold));

        if needs_cosign {
            let our_sig = self.keychain.sign(&unit);

            self.graph.record_sig(
                unit.round,
                unit.creator,
                self.own_id,
                our_sig,
                &self.keychain,
            );

            let entry = self
                .graph
                .entry(unit.round, unit.creator)
                .expect("just signed at this slot");

            self.send_entry(Recipient::Everyone, entry);
        }
    }

    async fn try_create_unit(&mut self) {
        // After a wipe-and-restore, peers can fill our slot via
        // anti-entropy before we've reached the create-timer arm. Adopt
        // those slots — building a *different* unit at the same
        // `(round, own_id)` would fork against peers that already
        // endorsed the old one.
        while self.graph.entry(self.next_round, self.own_id).is_some() {
            self.next_round += 1;
        }

        let Some(parents) = self.graph.parents_for(self.next_round) else {
            return;
        };

        let round = self.next_round;

        let unit = Unit {
            session: self.graph.session(),
            round,
            creator: self.own_id,
            parents,
            data: self.data_provider.get_data().await,
        };

        let sig = self.keychain.sign(&unit);

        match self.graph.insert_unit(unit.clone()) {
            InsertOutcome::Accepted => {}
            outcome => panic!("newly built round-{round} unit must insert: {outcome:?}"),
        }

        // Crash barrier: persist the unit + our self-sig before
        // broadcasting. On restart we'd otherwise be free to build a
        // *different* unit at this slot from a fresh data_provider
        // draw, and peers who saw the original message would consider
        // us a forker.
        self.graph
            .record_sig(round, self.own_id, self.own_id, sig, &self.keychain);

        self.network.send(
            Recipient::Everyone,
            Message::Unit {
                unit,
                sigs: BTreeMap::from([(self.own_id, sig)]),
            },
        );

        self.next_round += 1;
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
