use std::collections::BTreeMap;
use std::time::Duration;

use async_channel::Sender;
use picomint_core::PeerId;
use picomint_core::secp256k1::schnorr;
use tokio::time::{Instant, MissedTickBehavior, interval, sleep_until};

use crate::backup::DynBackup;
use crate::data_provider::DataProvider;
use crate::extender::Extender;
use crate::graph::{Graph, InsertOutcome, SigOutcome};
use crate::keychain::Keychain;
use crate::network::{DynNetwork, Message, Recipient};
use crate::unit::{Round, Unit, UnitData};

/// How often each peer broadcasts a `Status` summary of its per-creator
/// frontier so peers behind on any creator can be filled in by the
/// recipient's gap-fill response (`handle_status`).
const STATUS_INTERVAL: Duration = Duration::from_millis(250);

/// Drive a single peer's growth indefinitely.
///
/// Three concurrent arms drive the loop:
///
/// - **Inbound messages** are verified and folded into the local `Graph`
///   via `process_message`. Confirmation events fan into the local
///   `Extender`; whenever it elects a head and extracts a batch, every
///   unit's `data` items are forwarded as `(round, creator, datum)`
///   triples on `ordered_tx`.
/// - **`Status` broadcast** every `STATUS_INTERVAL`: anti-entropy summary
///   of our per-creator frontier; receivers reply with the gap they can
///   fill (`handle_status`).
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
#[allow(clippy::too_many_arguments)]
pub async fn run<D: UnitData, P: DataProvider<D>>(
    own_id: PeerId,
    mut graph: Graph<D>,
    keychain: Keychain,
    network: DynNetwork<Message<D>>,
    backup: DynBackup<D>,
    mut data_provider: P,
    ordered_tx: Sender<(Round, PeerId, D)>,
    unit_delay: Box<dyn Fn(Round) -> Duration + Send + 'static>,
) {
    let mut extender = Extender::new(graph.threshold());

    // The next round we'll attempt to create a unit at. Starts at 0 (no
    // genesis pre-population — round 0 is created and disseminated like
    // any other round). Bumped past every round we restore from backup.
    let mut next_round_to_create: Round = 0;

    for entry in backup.load() {
        // The backup table is shared across sessions; entries from a
        // previous (uncompleted) session would otherwise pollute this
        // session's graph. Filter strictly here so the engine — and the
        // extender, which can't tolerate cross-session unit hashes —
        // only ever sees this session's units.
        if entry.unit().session != graph.session() {
            continue;
        }

        let unit = entry.unit().clone();
        if unit.creator == own_id {
            next_round_to_create = next_round_to_create.max(unit.round + 1);
        }
        graph.restore_entry(entry);
        emit_batches(&mut extender, &ordered_tx, unit);
    }

    let mut status_timer = interval(STATUS_INTERVAL);
    status_timer.set_missed_tick_behavior(MissedTickBehavior::Delay);

    // Variable-delay deadline rather than a fixed-period `Interval` so
    // the engine honours the per-round delay closure: after each create
    // attempt we recompute the next deadline from `unit_delay(round)`.
    let mut next_create_at = Instant::now();

    loop {
        tokio::select! {
            maybe_msg = network.receive() => {
                let Some((sender, msg)) = maybe_msg else { return };

                process_message(
                    &mut graph,
                    &keychain,
                    &network,
                    &backup,
                    own_id,
                    sender,
                    msg,
                    &mut extender,
                    &ordered_tx,
                );
            }
            _ = status_timer.tick() => {
                broadcast_status(&graph, &network);
            }
            _ = sleep_until(next_create_at) => {
                next_round_to_create = advance_round(
                    &mut graph,
                    &keychain,
                    &network,
                    &backup,
                    own_id,
                    next_round_to_create,
                    &mut extender,
                    &ordered_tx,
                    &mut data_provider,
                ).await;

                next_create_at = Instant::now() + unit_delay(next_round_to_create);
            }
        }
    }
}

/// Persist the current state of the slot we just mutated. Called after
/// every `insert_unit → Accepted` and every `record_sig` that recorded
/// (or confirmed) a signature.
fn save_slot<D: UnitData>(backup: &DynBackup<D>, graph: &Graph<D>, round: Round, creator: PeerId) {
    backup.save(
        graph
            .entry(round, creator)
            .expect("save_slot called after a successful mutation"),
    );
}

/// Apply one inbound `Message` to local state. Unit creation is driven
/// separately on the `create_timer` arm so the rate limit is enforced.
#[allow(clippy::too_many_arguments)]
fn process_message<D: UnitData>(
    graph: &mut Graph<D>,
    keychain: &Keychain,
    network: &DynNetwork<Message<D>>,
    backup: &DynBackup<D>,
    own_id: PeerId,
    sender: PeerId,
    msg: Message<D>,
    extender: &mut Extender<D>,
    ordered_tx: &Sender<(Round, PeerId, D)>,
) {
    match msg {
        Message::Propose { unit, sig } => {
            handle_propose(
                graph, keychain, network, backup, own_id, unit, sig, extender, ordered_tx,
            );
        }
        Message::Ack {
            round,
            creator,
            sig,
        } => {
            handle_ack(
                graph, keychain, backup, round, creator, sender, sig, extender, ordered_tx,
            );
        }
        Message::Confirmed { unit, sigs } => {
            handle_confirmed(graph, keychain, backup, unit, sigs, extender, ordered_tx);
        }
        Message::Status { highest } => {
            handle_status(graph, network, own_id, sender, highest);
        }
    }
}

/// Broadcast our per-creator frontier — the highest round at which we hold
/// an entry, found by walking up `(creator, k)` until the first miss — to
/// every peer. Receivers compare against their own frontier in
/// `handle_status` and unicast back the units in any gap they can fill.
fn broadcast_status<D: UnitData>(graph: &Graph<D>, network: &DynNetwork<Message<D>>) {
    let highest: BTreeMap<PeerId, Round> = graph
        .peer_ids()
        .map(|creator| {
            let mut round: Round = 0;
            while graph.entry(round + 1, creator).is_some() {
                round += 1;
            }
            (creator, round)
        })
        .collect();

    network.send(Recipient::Everyone, Message::Status { highest });
}

/// Respond to a peer's `Status` by unicasting what we hold per creator.
///
/// We walk a priority queue keyed by lowest round next, mutating the
/// requester's reported `highest` map: each step we look at the
/// `(peer, round)` with the smallest round, send what we have at
/// `(round, peer)`, and bump that peer's pending round. Round-major
/// ordering matters: a unit at `(R, c)` has parents at `(R-1, *)`
/// spanning multiple creators, so naive per-creator walks would drop
/// most rounds on the first attempt — `insert_unit` requires confirmed
/// parents.
///
/// The message kind we send depends on what we hold and whether the
/// requester already has the unit body:
///
/// - **Confirmed at us**: `Confirmed { unit, sigs }`. At `round > their_top`
///   this delivers the unit + threshold sigs in one shot. At
///   `round == their_top` it's redundant when the requester is already
///   confirmed, but it's load-bearing when they hold the unit unconfirmed
///   — without it they'd be stuck (a `Confirmed` at `their_top + 1` can't
///   insert because parents at `their_top` aren't yet confirmed there).
/// - **Unconfirmed, we are the creator**: `Propose` with our recorded
///   creator sig — receiver runs `handle_propose`, signs, and acks.
/// - **Unconfirmed, we previously co-signed**: `Ack` with our sig.
fn handle_status<D: UnitData>(
    graph: &Graph<D>,
    network: &DynNetwork<Message<D>>,
    own_id: PeerId,
    requester: PeerId,
    mut highest: BTreeMap<PeerId, Round>,
) {
    while let Some((peer, round)) = highest
        .iter()
        .min_by_key(|&(_, &r)| r)
        .map(|(&p, &r)| (p, r))
    {
        if let Some(entry) = graph.entry(round, peer) {
            if entry.is_confirmed(graph.threshold()) {
                network.send(
                    Recipient::Peer(requester),
                    Message::Confirmed {
                        unit: entry.unit().clone(),
                        sigs: entry.sigs().clone(),
                    },
                );
            } else if peer == own_id {
                network.send(
                    Recipient::Peer(requester),
                    Message::Propose {
                        unit: entry.unit().clone(),
                        sig: *entry
                            .sigs()
                            .get(&own_id)
                            .expect("creator self-sig is recorded at unit creation"),
                    },
                );
            } else if let Some(&own_sig) = entry.sigs().get(&own_id) {
                network.send(
                    Recipient::Peer(requester),
                    Message::Ack {
                        round: entry.unit().round,
                        creator: entry.unit().creator,
                        sig: own_sig,
                    },
                );
            }

            highest.insert(peer, round + 1);
        } else {
            highest.remove(&peer);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_propose<D: UnitData>(
    graph: &mut Graph<D>,
    keychain: &Keychain,
    network: &DynNetwork<Message<D>>,
    backup: &DynBackup<D>,
    own_id: PeerId,
    unit: Unit<D>,
    creator_sig: schnorr::Signature,
    extender: &mut Extender<D>,
    ordered_tx: &Sender<(Round, PeerId, D)>,
) {
    if !keychain.verify(&unit.hash(), &creator_sig, unit.creator) {
        return;
    }

    if graph.insert_unit(unit.clone()) != InsertOutcome::Accepted {
        return;
    }

    save_slot(backup, graph, unit.round, unit.creator);

    if graph.record_sig(
        unit.round,
        unit.creator,
        unit.creator,
        creator_sig,
        keychain,
    ) == SigOutcome::Confirmed
    {
        on_confirm(graph, extender, ordered_tx, unit.round, unit.creator);
    }

    save_slot(backup, graph, unit.round, unit.creator);

    let our_sig = keychain.sign(&unit.hash());

    if graph.record_sig(unit.round, unit.creator, own_id, our_sig, keychain)
        == SigOutcome::Confirmed
    {
        on_confirm(graph, extender, ordered_tx, unit.round, unit.creator);
    }

    save_slot(backup, graph, unit.round, unit.creator);

    network.send(
        Recipient::Everyone,
        Message::Ack {
            round: unit.round,
            creator: unit.creator,
            sig: our_sig,
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn handle_ack<D: UnitData>(
    graph: &mut Graph<D>,
    keychain: &Keychain,
    backup: &DynBackup<D>,
    round: Round,
    creator: PeerId,
    signer: PeerId,
    sig: schnorr::Signature,
    extender: &mut Extender<D>,
    ordered_tx: &Sender<(Round, PeerId, D)>,
) {
    let outcome = graph.record_sig(round, creator, signer, sig, keychain);

    if outcome == SigOutcome::Discarded {
        return;
    }

    save_slot(backup, graph, round, creator);

    if outcome == SigOutcome::Confirmed {
        on_confirm(graph, extender, ordered_tx, round, creator);
    }
}

fn handle_confirmed<D: UnitData>(
    graph: &mut Graph<D>,
    keychain: &Keychain,
    backup: &DynBackup<D>,
    unit: Unit<D>,
    sigs: BTreeMap<PeerId, schnorr::Signature>,
    extender: &mut Extender<D>,
    ordered_tx: &Sender<(Round, PeerId, D)>,
) {
    // Best-effort insert. If parents aren't available the slot stays
    // empty and the per-sig record_sig calls below are no-ops; the
    // sender will rebroadcast (or another peer will) once we've caught up.
    if graph.insert_unit(unit.clone()) == InsertOutcome::Accepted {
        save_slot(backup, graph, unit.round, unit.creator);
    }

    for (signer, sig) in sigs {
        let outcome = graph.record_sig(unit.round, unit.creator, signer, sig, keychain);

        if outcome == SigOutcome::Discarded {
            continue;
        }

        save_slot(backup, graph, unit.round, unit.creator);

        if outcome == SigOutcome::Confirmed {
            on_confirm(graph, extender, ordered_tx, unit.round, unit.creator);
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn advance_round<D: UnitData, P: DataProvider<D>>(
    graph: &mut Graph<D>,
    keychain: &Keychain,
    network: &DynNetwork<Message<D>>,
    backup: &DynBackup<D>,
    own_id: PeerId,
    mut next_round_to_create: Round,
    extender: &mut Extender<D>,
    ordered_tx: &Sender<(Round, PeerId, D)>,
    data_provider: &mut P,
) -> Round {
    // After a wipe-and-restore, peers can fill our slot via `Status`
    // gap-fill (a `Confirmed` carrying our own prior round-N unit) before
    // we've reached the create-timer arm. Adopt those slots — building a
    // *different* unit at the same `(round, own_id)` would fork against
    // peers that already endorsed the old one.
    while graph.entry(next_round_to_create, own_id).is_some() {
        next_round_to_create += 1;
    }

    if let Some(parents) = graph.parents_for(next_round_to_create, own_id) {
        let round = next_round_to_create;

        let unit = Unit {
            session: graph.session(),
            round,
            creator: own_id,
            parents,
            data: data_provider.get_data().await,
        };

        tracing::info!(
            round = round,
            creator = %own_id,
            data_len = unit.data.len(),
            "created unit",
        );

        let sig = keychain.sign(&unit.hash());

        assert_eq!(
            graph.insert_unit(unit.clone()),
            InsertOutcome::Accepted,
            "newly built round-{round} unit must insert",
        );

        if graph.record_sig(round, own_id, own_id, sig, keychain) == SigOutcome::Confirmed {
            on_confirm(graph, extender, ordered_tx, round, own_id);
        }

        // Crash barrier: persist the unit + our self-sig before
        // broadcasting. On restart we'd otherwise be free to build a
        // *different* unit at this slot from a fresh data_provider draw,
        // and peers who saw the original Propose would consider us a
        // forker.
        save_slot(backup, graph, round, own_id);

        network.send(Recipient::Everyone, Message::Propose { unit, sig });

        next_round_to_create += 1;
    }

    next_round_to_create
}

fn on_confirm<D: UnitData>(
    graph: &Graph<D>,
    extender: &mut Extender<D>,
    ordered_tx: &Sender<(Round, PeerId, D)>,
    round: Round,
    creator: PeerId,
) {
    let unit = graph
        .entry(round, creator)
        .expect("just confirmed at this slot")
        .unit()
        .clone();

    emit_batches(extender, ordered_tx, unit);
}

fn emit_batches<D: UnitData>(
    extender: &mut Extender<D>,
    ordered_tx: &Sender<(Round, PeerId, D)>,
    unit: Unit<D>,
) {
    for batch in extender.add_unit(unit) {
        for u in batch {
            for d in &u.data {
                ordered_tx
                    .try_send((u.round, u.creator, d.clone()))
                    .expect("unbounded channel never refuses");
            }
        }
    }
}
