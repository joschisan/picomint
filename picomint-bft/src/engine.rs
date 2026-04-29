use std::collections::BTreeMap;
use std::time::Duration;

use async_channel::Sender;
use picomint_core::PeerId;
use picomint_core::secp256k1::schnorr;
use tokio::time::{Instant, sleep_until};

use crate::backup::DynBackup;
use crate::data::DataProvider;
use crate::extender::Extender;
use crate::graph::{Graph, InsertOutcome, SigOutcome};
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
/// - **Inbound messages** are verified and folded into the local `Graph`
///   via `process_message`. Confirmation events fan into the local
///   `Extender`; whenever it elects a head and extracts a batch, every
///   unit's `data` items are forwarded as `(round, creator, datum)`
///   triples on `ordered_tx`.
/// - **Anti-entropy** every `ANTI_ENTROPY_INTERVAL`: push the highest
///   known entry per peer to everyone, and pull (one `Request` per peer)
///   for the lowest round we don't yet have confirmed. Push refills sig
///   deficits at slots receivers already hold; pull fills slots receivers
///   are missing entirely.
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
    let mut extender = Extender::new(graph.num_peers());

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
            panic!("backup session does not match graph session");
        }

        if entry.unit().creator == own_id {
            next_round_to_create = next_round_to_create.max(entry.unit().round + 1);
        }

        graph.restore_entry(entry.clone());

        if entry.is_confirmed(graph.threshold()) {
            emit_batches(&mut extender, &ordered_tx, entry.unit().clone());
        }
    }

    let mut next_create_at = Instant::now();
    let mut next_broadcast_at = Instant::now();

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

            _ = sleep_until(next_broadcast_at) => {
                broadcast_anti_entropy(&graph, &network);

                next_broadcast_at = Instant::now() + ANTI_ENTROPY_INTERVAL;
            }
        }
    }
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
        Message::Unit { unit, sigs } => {
            handle_unit(
                graph, keychain, network, backup, own_id, unit, sigs, extender, ordered_tx,
            );
        }
        Message::Request { round, creator } => {
            handle_request(graph, network, sender, round, creator);
        }
    }
}

/// Per-cycle anti-entropy. Two legs, both bounded at one message per
/// peer in the federation:
///
/// - **Push**: for every peer, send our highest known entry for that
///   peer (with the sigs we currently hold) to everyone. Receivers
///   union sigs into their own view; if their copy was below threshold
///   it may now confirm. New entries at higher rounds are accepted by
///   strict insert iff their parents are already confirmed locally;
///   otherwise dropped, to be re-delivered after pull has caught the
///   receiver up.
///
/// - **Pull**: for every peer, send a `Request` for the lowest round
///   where we don't yet hold that peer's slot confirmed. Recipients
///   that hold the slot reply with their entry. Idempotent — the same
///   round is re-requested next cycle until it confirms.
fn broadcast_anti_entropy<D: UnitData>(graph: &Graph<D>, network: &DynNetwork<Message<D>>) {
    for creator in graph.peer_ids() {
        if let Some(entry) = graph.highest_entry(creator) {
            network.send(
                Recipient::Everyone,
                Message::Unit {
                    unit: entry.unit().clone(),
                    sigs: entry.sigs().clone(),
                },
            );
        }

        let round = graph.lowest_unconfirmed_round(creator);

        network.send(Recipient::Everyone, Message::Request { round, creator });
    }
}

/// Respond to a `Request` for `(round, creator)` by unicasting our entry
/// at that slot — body plus all sigs we currently hold — back to the
/// requester. The requester is asking either because the slot is absent
/// from their graph or present-but-unconfirmed locally.
fn handle_request<D: UnitData>(
    graph: &Graph<D>,
    network: &DynNetwork<Message<D>>,
    requester: PeerId,
    round: Round,
    creator: PeerId,
) {
    let Some(entry) = graph.entry(round, creator) else {
        return;
    };

    network.send(
        Recipient::Peer(requester),
        Message::Unit {
            unit: entry.unit().clone(),
            sigs: entry.sigs().clone(),
        },
    );
}

/// Apply one inbound `Unit` message:
///
/// - Validate (creator-sig present, sigs cap, every carried sig verifies).
/// - Insert (strict — drops anything whose parents aren't confirmed
///   locally; the periodic anti-entropy will refill the prerequisites
///   and the unit will arrive again on a later cycle).
/// - Merge each carried sig; if the slot crosses threshold it confirms
///   immediately (no cascade — strict insert means parents are already
///   confirmed).
/// - If we haven't co-signed yet, sign and rebroadcast so peers union
///   our contribution.
#[allow(clippy::too_many_arguments)]
fn handle_unit<D: UnitData>(
    graph: &mut Graph<D>,
    keychain: &Keychain,
    network: &DynNetwork<Message<D>>,
    backup: &DynBackup<D>,
    own_id: PeerId,
    unit: Unit<D>,
    sigs: BTreeMap<PeerId, schnorr::Signature>,
    extender: &mut Extender<D>,
    ordered_tx: &Sender<(Round, PeerId, D)>,
) {
    // Creator's sig must be in the bundle: it binds the body to its
    // claimed author. Without this check, a Byzantine peer could send
    // a fabricated body at someone else's slot signed only with their
    // own key, blocking the legitimate creator from inserting.
    if !sigs.contains_key(&unit.creator) {
        return;
    }

    // Cap verify cost: a malicious peer can't make us check more sigs
    // than a fully-confirmed unit would carry.
    if sigs.len() > graph.threshold() {
        return;
    }

    // Every carried sig must verify against the unit under its claimed signer.
    if !sigs
        .iter()
        .all(|(signer, sig)| keychain.verify(&unit, sig, *signer))
    {
        return;
    }

    // Insert the body. `Duplicate` is fine — we already had it.
    // `WrongSession`, `OversizedData`, `InvalidParents`, `MissingParents`
    // all drop; `MissingParents` is the expected steady-state miss when
    // anti-entropy hasn't yet caught us up to the broadcasting peer.
    match graph.insert_unit(unit.clone()) {
        InsertOutcome::Accepted(entry) => backup.save(&entry),
        InsertOutcome::Duplicate => {}
        _ => return,
    }

    // Merge each carried sig.
    for (signer, sig) in &sigs {
        record_sig_and_emit(
            graph,
            keychain,
            backup,
            extender,
            ordered_tx,
            unit.round,
            unit.creator,
            *signer,
            *sig,
        );
    }

    // If our own sig isn't yet at this slot — and the slot hasn't
    // already confirmed — sign now and rebroadcast.
    if let Some(entry) = graph.entry(unit.round, unit.creator)
        && !entry.sigs().contains_key(&own_id)
        && !entry.is_confirmed(graph.threshold())
    {
        let our_sig = keychain.sign(&unit);

        record_sig_and_emit(
            graph,
            keychain,
            backup,
            extender,
            ordered_tx,
            unit.round,
            unit.creator,
            own_id,
            our_sig,
        );

        let entry = graph
            .entry(unit.round, unit.creator)
            .expect("just signed at this slot");

        network.send(
            Recipient::Everyone,
            Message::Unit {
                unit: entry.unit().clone(),
                sigs: entry.sigs().clone(),
            },
        );
    }
}

/// Record a co-sig and forward the entry to the backup + extender.
/// Strict insert means a confirmation transition is local to this slot
/// — there's no cascade to higher rounds.
#[allow(clippy::too_many_arguments)]
fn record_sig_and_emit<D: UnitData>(
    graph: &mut Graph<D>,
    keychain: &Keychain,
    backup: &DynBackup<D>,
    extender: &mut Extender<D>,
    ordered_tx: &Sender<(Round, PeerId, D)>,
    round: Round,
    creator: PeerId,
    signer: PeerId,
    sig: schnorr::Signature,
) {
    match graph.record_sig(round, creator, signer, sig, keychain) {
        SigOutcome::Confirmed(entry) => {
            backup.save(&entry);
            emit_batches(extender, ordered_tx, entry.unit().clone());
        }
        SigOutcome::Recorded(entry) => backup.save(&entry),
        SigOutcome::Discarded => {}
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

    if let Some(parents) = graph.parents_for(next_round_to_create) {
        let round = next_round_to_create;

        let unit = Unit {
            session: graph.session(),
            round,
            creator: own_id,
            parents,
            data: data_provider.get_data().await,
        };

        let sig = keychain.sign(&unit);

        match graph.insert_unit(unit.clone()) {
            InsertOutcome::Accepted(_) => {}
            outcome => panic!("newly built round-{round} unit must insert: {outcome:?}"),
        }

        // Crash barrier: persist the unit + our self-sig before
        // broadcasting. On restart we'd otherwise be free to build a
        // *different* unit at this slot from a fresh data_provider draw,
        // and peers who saw the original message would consider us a
        // forker.
        record_sig_and_emit(
            graph, keychain, backup, extender, ordered_tx, round, own_id, own_id, sig,
        );

        network.send(
            Recipient::Everyone,
            Message::Unit {
                unit,
                sigs: BTreeMap::from([(own_id, sig)]),
            },
        );

        next_round_to_create += 1;
    }

    next_round_to_create
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
