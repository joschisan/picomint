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

/// Periodic own-slot push interval. Pull is demand-driven, not periodic.
const ANTI_ENTROPY_INTERVAL: Duration = Duration::from_secs(1);

/// Drive a single peer's growth indefinitely. Runs until the caller
/// drops the task or the network closes. `unit_delay(round)` rate-
/// limits creation — typically `0` per round in steady state, with
/// exponential slowdown past `rounds_per_session` so an unterminating
/// session can't grow rounds unboundedly.
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

    fn broadcast_anti_entropy(&self) {
        if let Some(entry) = self.graph.highest_entry(self.own_id) {
            self.send_unit(Recipient::Everyone, entry);
        }
    }

    /// Reply with `SignedUnit` only when the slot is locally confirmed.
    /// A sub-threshold bundle would be unsafe — the receiver overwrites
    /// its entry on the strength of the threshold proof.
    fn handle_request(&self, requester: PeerId, round: Round, creator: PeerId) {
        let Some(entry) = self.graph.entry(round, creator) else {
            return;
        };

        if !entry.is_confirmed(self.graph.threshold()) {
            return;
        }

        // `record_cosig` caps at threshold, so `entry.cosigs` already
        // holds exactly `2f` cosigs — no trim needed.
        self.network.send(
            Recipient::Peer(requester),
            Message::SignedUnit {
                unit: entry.unit().clone(),
                sig: *entry.sig(),
                cosigs: entry.cosigs().clone(),
            },
        );
    }

    /// Verify, lax-insert, cosign-on-first-sight (own broadcast), then
    /// demand-pull any not-yet-extended parents from the sender.
    fn handle_unit(&mut self, sender: PeerId, unit: Unit<D>, sig: schnorr::Signature) {
        if !self
            .keychain
            .verify(self.graph.session(), &unit, &sig, unit.creator)
        {
            return;
        }

        let unit_round = unit.round;
        let unit_creator = unit.creator;
        let parents: Vec<PeerId> = unit.parents.iter().copied().collect();

        // We don't cosign our own slot (the creator sig is already ours).
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

        // Re-issuing on every receive (fresh or duplicate) is the
        // retry mechanism for dropped `Request` messages.
        if let Some(parent_round) = unit_round.checked_sub(1) {
            for parent_creator in parents {
                if !self.graph.is_extended(parent_round, parent_creator) {
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

    /// Atomically install/overwrite from the threshold proof, then
    /// demand-pull any not-yet-extended parents from the sender. No
    /// rebroadcast, no cosig fan-out — this is a pull-driven event.
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
                if !self.graph.is_extended(parent_round, parent_creator) {
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

    /// Verify and merge if we hold the body; otherwise demand-pull
    /// the body from the signer and drop the cosig (the response
    /// will carry it back inside the SignedUnit).
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

    /// Derive the next round to attempt creation at from our highest
    /// own-slot. After wipe-and-restore, peers may have refilled our
    /// slot via anti-entropy; `highest_entry` accounts for that, so
    /// we resume at `highest + 1` rather than re-forking it.
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

        // Crash barrier: persist before broadcasting, otherwise a
        // restart would let us build a *different* unit at this slot
        // from a fresh data_provider draw — peers that saw the
        // original would consider us a forker.
        let entry = self
            .graph
            .insert_unit(unit, sig, BTreeMap::new(), &self.keychain)
            .unwrap_or_else(|| panic!("newly built round-{round} unit must insert"));

        self.send_unit(Recipient::Everyone, &entry);
    }

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
