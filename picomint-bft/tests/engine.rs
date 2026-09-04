//! End-to-end tests of the per-peer engine driving DAG growth, order
//! extraction, and emission of ordered items through the bft engine's
//! `ordered_tx` channel over a mock channel-based network. N engines
//! spawn as tasks, each owns a per-peer in-memory `Database`, a
//! `Keychain`, a `MockChannel`, and the receiving end of its own
//! `ordered_tx`. Each peer feeds its own per-unit payload of one
//! `u64`. The engines never stop on their own — each test drains the
//! peers' `ordered_rx` until `ROUND_LIMIT` is reached, then aborts the
//! engine tasks and asserts every peer observed the same total order
//! of `(creator, item)` pairs. The Byzantine tests replace peer 0 with
//! hand-built forked units covering the three fork outcomes — split
//! votes (both branches decide 0), unanimous votes (commit), and a
//! forked voter one round up — plus units with spoofed parent
//! positions; the replay
//! test restarts an engine on its persisted units and asserts the
//! emission sequence reproduces exactly.

use std::collections::BTreeMap;
use std::future::pending;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_channel::{Receiver, Sender};
use async_trait::async_trait;
use picomint_bft::{
    DataProvider, Engine, INetwork, Keychain, Message, Recipient, Round, Unit, UnitEnvelope,
    UnitHash,
};
use picomint_core::secp256k1::{Keypair, SECP256K1, rand};
use picomint_core::{NumPeers, PeerId};
use picomint_encoding::Encodable;
use picomint_redb::{Database, table};
use rand::Rng;
use tokio::task::JoinHandle;
use tokio::time::timeout;

table!(BftUnits, UnitHash => UnitEnvelope<u64>, "bft-units");

/// Per-recipient probability of silently dropping a message in the mock
/// network, used by the 4-peer tests. Each unicast send and each
/// fan-out leg of a broadcast rolls independently.
const DROP_RATE: f64 = 0.10;

/// Base one-way latency applied to every delivered message in the mock
/// network. Each send adds `BASE_LATENCY` plus a uniform jitter in
/// `[0, JITTER]` before the message lands in the recipient's inbox.
const BASE_LATENCY: Duration = Duration::from_millis(25);
const JITTER: Duration = Duration::from_millis(15);

/// Channel-backed mock network. Each peer holds one `MockChannel`.
/// Built via [`MockChannel::mesh`] for an N-peer fully-connected mesh;
/// sends drop with probability `DROP_RATE` per recipient leg to simulate
/// an unreliable network. Implements [`INetwork`].
struct MockChannel {
    own_id: PeerId,
    drop_rate: f64,
    senders: BTreeMap<PeerId, Sender<(PeerId, Message<u64>)>>,
    rx: Receiver<(PeerId, Message<u64>)>,
}

impl MockChannel {
    /// Build a fully-connected mesh of channels, one per peer in `n`,
    /// each dropping sends with probability `drop_rate` per leg.
    fn mesh(n: NumPeers, drop_rate: f64) -> BTreeMap<PeerId, MockChannel> {
        let mut receivers = BTreeMap::new();
        let mut senders = BTreeMap::new();

        for peer in n.peer_ids() {
            let (tx, rx) = async_channel::unbounded();
            senders.insert(peer, tx);
            receivers.insert(peer, rx);
        }

        n.peer_ids()
            .map(|own_id| {
                let rx = receivers.remove(&own_id).expect("inserted above");
                let channel = MockChannel {
                    own_id,
                    drop_rate,
                    senders: senders.clone(),
                    rx,
                };
                (own_id, channel)
            })
            .collect()
    }

    /// Deliver `msg` to `recipient` directly, bypassing the drop roll
    /// and latency of [`INetwork::send`]. The Byzantine tests use this
    /// to place forged units in chosen inboxes deterministically.
    fn deliver(&self, recipient: PeerId, msg: Message<u64>) {
        self.senders
            .get(&recipient)
            .expect("mesh built above")
            .try_send((self.own_id, msg))
            .expect("mock channels are unbounded");
    }
}

fn delayed_send(sender: Sender<(PeerId, Message<u64>)>, from: PeerId, msg: Message<u64>) {
    let jitter = Duration::from_micros(rand::thread_rng().gen_range(0..=JITTER.as_micros() as u64));
    let delay = BASE_LATENCY + jitter;

    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        let _ = sender.try_send((from, msg));
    });
}

#[async_trait]
impl INetwork<u64> for MockChannel {
    fn send(&self, recipient: Recipient, msg: Message<u64>) {
        match recipient {
            Recipient::Everyone => {
                for (peer, sender) in &self.senders {
                    if *peer == self.own_id {
                        continue;
                    }

                    if rand::random::<f64>() < self.drop_rate {
                        continue;
                    }

                    delayed_send(sender.clone(), self.own_id, msg.clone());
                }
            }
            Recipient::Peer(to) => {
                assert_ne!(to, self.own_id, "MockChannel send must not target self");

                let sender = self
                    .senders
                    .get(&to)
                    .expect("recipient must be a known peer");

                if rand::random::<f64>() < self.drop_rate {
                    return;
                }

                delayed_send(sender.clone(), self.own_id, msg);
            }
        }
    }

    async fn receive(&self) -> Option<(PeerId, Message<u64>)> {
        self.rx.recv().await.ok()
    }

    async fn receive_from_peer(&self, _peer: PeerId) -> Option<Message<u64>> {
        unimplemented!(
            "MockChannel multiplexes inbound traffic on a single receiver; \
             per-peer reads are only meaningful for round-robin DKG, which \
             picomint-bft doesn't have"
        )
    }
}

/// Sink network for an engine replaying from disk in isolation: sends
/// vanish and no message ever arrives.
struct NullNetwork;

#[async_trait]
impl INetwork<u64> for NullNetwork {
    fn send(&self, _recipient: Recipient, _msg: Message<u64>) {}

    async fn receive(&self) -> Option<(PeerId, Message<u64>)> {
        pending().await
    }

    async fn receive_from_peer(&self, _peer: PeerId) -> Option<Message<u64>> {
        pending().await
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is past UNIX epoch")
        .as_millis() as u64
}

struct TimestampDataProvider;

#[async_trait]
impl DataProvider<u64> for TimestampDataProvider {
    fn get_data(&mut self) -> Vec<u64> {
        vec![now_ms()]
    }

    async fn wait_for_data(&mut self) {
        // Data is always available, so every message-driven `get_data`
        // returns items and the wake arm never needs to fire.
        pending().await
    }
}

const N_PEERS: usize = 4;
const ROUND_LIMIT: Round = 100;
const SESSION: u32 = 0;

fn build_keychains(n: NumPeers) -> BTreeMap<PeerId, Keychain> {
    let keypairs: BTreeMap<PeerId, Keypair> = n
        .peer_ids()
        .map(|id| (id, Keypair::new(SECP256K1, &mut rand::thread_rng())))
        .collect();

    let pubkeys: BTreeMap<_, _> = keypairs
        .iter()
        .map(|(id, kp)| (*id, kp.x_only_public_key().0))
        .collect();

    keypairs
        .into_iter()
        .map(|(id, kp)| (id, Keychain::new(kp, pubkeys.clone())))
        .collect()
}

/// Engines spawned by [`spawn_engines`], plus the handles a test needs
/// to observe and later restart them.
struct Engines {
    handles: Vec<JoinHandle<()>>,
    ordered_rxs: BTreeMap<PeerId, Receiver<(Round, PeerId, u64)>>,
    dbs: BTreeMap<PeerId, Database>,
}

/// Spawn one engine per remaining entry of `channels`, each on a fresh
/// in-memory database. Byzantine tests remove the forker's channel
/// from the mesh beforehand.
fn spawn_engines(
    n: NumPeers,
    keychains: &BTreeMap<PeerId, Keychain>,
    channels: BTreeMap<PeerId, MockChannel>,
) -> Engines {
    let mut engines = Engines {
        handles: Vec::new(),
        ordered_rxs: BTreeMap::new(),
        dbs: BTreeMap::new(),
    };

    for (peer, channel) in channels {
        let db = Database::open_in_memory();

        engines.dbs.insert(peer, db.clone());

        let (ordered_tx, ordered_rx) = async_channel::unbounded();
        engines.ordered_rxs.insert(peer, ordered_rx);

        let engine = Engine::new(
            peer,
            SESSION,
            n,
            db,
            keychains.get(&peer).expect("built above").clone(),
            Arc::new(channel),
            TimestampDataProvider,
            ordered_tx,
            BftUnits,
        );

        engines.handles.push(tokio::spawn(engine.run()));
    }

    engines
}

/// Drain every peer's ordered stream until the first item above
/// `ROUND_LIMIT`, and return the observed `(creator, item)` sequences.
async fn collect_sequences(
    ordered_rxs: BTreeMap<PeerId, Receiver<(Round, PeerId, u64)>>,
) -> BTreeMap<PeerId, Vec<(PeerId, u64)>> {
    let mut reader_handles = Vec::new();

    for (peer, ordered_rx) in ordered_rxs {
        reader_handles.push(tokio::spawn(async move {
            let mut sequence: Vec<(PeerId, u64)> = Vec::new();

            while let Ok((round, creator, datum)) = ordered_rx.recv().await {
                if round > ROUND_LIMIT {
                    break;
                }
                sequence.push((creator, datum));
            }

            (peer, sequence)
        }));
    }

    let mut sequences = BTreeMap::new();

    for h in reader_handles {
        let (peer, seq) = h.await.expect("reader task panicked");
        sequences.insert(peer, seq);
    }

    sequences
}

/// Assert all sequences are non-empty and identical; return the shared
/// order.
fn assert_agreement(sequences: &BTreeMap<PeerId, Vec<(PeerId, u64)>>) -> Vec<(PeerId, u64)> {
    let reference = sequences
        .values()
        .next()
        .expect("at least one peer")
        .clone();
    assert!(!reference.is_empty(), "expected at least one ordered item");

    for (peer, seq) in sequences {
        assert_eq!(seq, &reference, "peer {peer} disagrees on total order");
    }

    reference
}

fn build_envelope(
    keychain: &Keychain,
    round: Round,
    creator: PeerId,
    parents: BTreeMap<PeerId, UnitHash>,
    data: u64,
) -> UnitEnvelope<u64> {
    let data = vec![data];

    let unit = Unit {
        round,
        creator,
        parents,
        data: Some(data.consensus_hash_sha256()),
    };

    UnitEnvelope {
        sig: keychain.sign(SESSION, &unit),
        unit,
        data,
    }
}

#[tokio::test]
async fn engines_agree_on_ordered_data() {
    let n = NumPeers::from(N_PEERS);
    let keychains = build_keychains(n);
    let channels = MockChannel::mesh(n, DROP_RATE);

    let engines = spawn_engines(n, &keychains, channels);

    let mut reader_handles = Vec::new();
    for (peer, ordered_rx) in engines.ordered_rxs {
        reader_handles.push(tokio::spawn(async move {
            let mut sequence: Vec<(PeerId, u64)> = Vec::new();
            let mut delays: Vec<u64> = Vec::new();

            while let Ok((round, creator, datum)) = ordered_rx.recv().await {
                if round > ROUND_LIMIT {
                    break;
                }
                delays.push(now_ms().saturating_sub(datum));
                sequence.push((creator, datum));
            }

            (peer, sequence, delays)
        }));
    }

    let mut sequences: BTreeMap<PeerId, Vec<(PeerId, u64)>> = BTreeMap::new();
    for h in reader_handles {
        let (peer, seq, delays) = h.await.expect("reader task panicked");

        let n_items = delays.len();
        let avg = delays.iter().sum::<u64>() as f64 / n_items as f64;
        let max = delays.iter().copied().max().unwrap_or(0);
        println!("peer {peer}: items={n_items} avg_delay={avg:.1}ms max_delay={max}ms");

        sequences.insert(peer, seq);
    }

    for h in engines.handles {
        h.abort();
    }

    assert_agreement(&sequences);
}

/// Peer 0 is Byzantine: it signs two different round-0 units (a fork)
/// and shows one branch to peers 1 and 2 and the other to peer 3, then
/// goes silent. The three honest engines must still agree on one total
/// order — the fork splits votes, so neither branch gathers a
/// 1-certificate and both decide 0, while the branches themselves may
/// still be swept as ancestry of honest heads.
#[tokio::test]
async fn engines_agree_under_forking_peer() {
    let n = NumPeers::from(N_PEERS);
    let keychains = build_keychains(n);
    let mut channels = MockChannel::mesh(n, DROP_RATE);

    let forker = PeerId::from(0u8);
    let forker_channel = channels.remove(&forker).expect("mesh built above");

    let engines = spawn_engines(n, &keychains, channels);

    for (data, recipients) in [(1u64, vec![1u8, 2u8]), (2u64, vec![3u8])] {
        let signed = build_envelope(&keychains[&forker], 0, forker, BTreeMap::new(), data);

        for recipient in recipients {
            forker_channel.deliver(PeerId::from(recipient), Message::Unit(signed.clone()));
        }
    }

    let sequences = collect_sequences(engines.ordered_rxs).await;

    for h in engines.handles {
        h.abort();
    }

    assert_agreement(&sequences);
}

/// Peer 0 forks its round-0 slot again, but this time shows *both*
/// branches to every honest peer before their engines start. All
/// honest peers extend both branches and deterministically reference —
/// and thus vote for — the lowest-hash one, so that branch gathers
/// certificates and the forked slot *commits*: the complementary
/// outcome to `engines_agree_under_forking_peer`, where split votes
/// force a skip. The unreferenced branch is never swept as ancestry,
/// so exactly one of the two forked payloads appears in the order.
#[tokio::test]
async fn engines_agree_when_fork_branch_commits() {
    let n = NumPeers::from(N_PEERS);
    let keychains = build_keychains(n);
    let mut channels = MockChannel::mesh(n, DROP_RATE);

    let forker = PeerId::from(0u8);
    let forker_channel = channels.remove(&forker).expect("mesh built above");

    for data in [1u64, 2u64] {
        let signed = build_envelope(&keychains[&forker], 0, forker, BTreeMap::new(), data);

        for recipient in [1u8, 2u8, 3u8] {
            forker_channel.deliver(PeerId::from(recipient), Message::Unit(signed.clone()));
        }
    }

    let engines = spawn_engines(n, &keychains, channels);

    let sequences = collect_sequences(engines.ordered_rxs).await;

    for h in engines.handles {
        h.abort();
    }

    let reference = assert_agreement(&sequences);

    let emitted_branches = [1u64, 2u64]
        .into_iter()
        .filter(|data| reference.contains(&(forker, *data)))
        .count();

    assert_eq!(emitted_branches, 1, "exactly one fork branch must commit");
}

/// Peer 0 turns Byzantine one round up: it publishes an honest-looking
/// round-0 unit to everyone, then signs two round-1 units over the
/// same parent set (differing payloads) and shows them to disjoint
/// halves of the mint — a forked *voter*. The vote and
/// certificate tallies over round 1 must handle the forker appearing
/// once per branch without double-counting its creator, and the honest
/// engines must still agree. The forker's round-0 unit is referenced
/// by every honest round-1 unit, so its payload must appear in the
/// order.
#[tokio::test]
async fn engines_agree_under_forking_voter() {
    let n = NumPeers::from(N_PEERS);
    let keychains = build_keychains(n);
    let mut channels = MockChannel::mesh(n, DROP_RATE);

    let forker = PeerId::from(0u8);
    let forker_channel = channels.remove(&forker).expect("mesh built above");

    let round_zero = build_envelope(&keychains[&forker], 0, forker, BTreeMap::new(), 0);

    for recipient in [1u8, 2u8, 3u8] {
        forker_channel.deliver(PeerId::from(recipient), Message::Unit(round_zero.clone()));
    }

    let engines = spawn_engines(n, &keychains, channels);

    // Harvest two honest round-0 hashes from the forker's inbox — from
    // honest round-0 broadcasts directly, or from the parent maps of
    // honest round-1 broadcasts when the originals were dropped.
    let mut harvested: BTreeMap<PeerId, UnitHash> = BTreeMap::new();

    while harvested.len() < 2 {
        let (_, msg) = forker_channel.rx.recv().await.expect("mesh alive");

        let Message::Unit(ev) = msg else {
            continue;
        };

        match ev.unit.round {
            0 => {
                harvested.insert(ev.unit.creator, ev.unit.hash());
            }
            1 => {
                harvested.extend(
                    ev.unit
                        .parents
                        .iter()
                        .filter(|(creator, _)| **creator != forker)
                        .map(|(creator, hash)| (*creator, *hash)),
                );
            }
            _ => {}
        }
    }

    let parents: BTreeMap<PeerId, UnitHash> = std::iter::once((forker, round_zero.unit.hash()))
        .chain(harvested.into_iter().take(n.threshold() - 1))
        .collect();

    for (data, recipients) in [(1u64, vec![1u8, 2u8]), (2u64, vec![3u8])] {
        let signed = build_envelope(&keychains[&forker], 1, forker, parents.clone(), data);

        for recipient in recipients {
            forker_channel.deliver(PeerId::from(recipient), Message::Unit(signed.clone()));
        }
    }

    let sequences = collect_sequences(engines.ordered_rxs).await;

    for h in engines.handles {
        h.abort();
    }

    let reference = assert_agreement(&sequences);

    assert!(
        reference.contains(&(forker, 0)),
        "the forker's round-0 payload must be swept as ancestry",
    );
}

/// Peer 0 attacks the parent position check: it publishes two round-0
/// branches to everyone, then two structurally malformed units — a
/// round-1 unit keying its own second branch under an honest peer's id
/// (creator spoof, a fabricated distinct-creator quorum), and a unit
/// claiming round 2 directly over round-0 parents (round skip, a
/// gap in extended rounds). Both pass admission (size, key set, sig)
/// but must never extend, so their payloads can never be referenced by
/// honest units nor swept into the order.
#[tokio::test]
async fn engines_ignore_spoofed_parent_positions() {
    let n = NumPeers::from(N_PEERS);
    let keychains = build_keychains(n);
    let mut channels = MockChannel::mesh(n, DROP_RATE);

    let forker = PeerId::from(0u8);
    let forker_channel = channels.remove(&forker).expect("mesh built above");

    let branch_a = build_envelope(&keychains[&forker], 0, forker, BTreeMap::new(), 0);
    let branch_b = build_envelope(&keychains[&forker], 0, forker, BTreeMap::new(), 1);

    for recipient in [1u8, 2u8, 3u8] {
        forker_channel.deliver(PeerId::from(recipient), Message::Unit(branch_a.clone()));
        forker_channel.deliver(PeerId::from(recipient), Message::Unit(branch_b.clone()));
    }

    let engines = spawn_engines(n, &keychains, channels);

    // Harvest two honest round-0 hashes from the forker's inbox — from
    // honest round-0 broadcasts directly, or from the parent maps of
    // honest round-1 broadcasts when the originals were dropped.
    let mut harvested: BTreeMap<PeerId, UnitHash> = BTreeMap::new();

    while harvested.len() < 2 {
        let (_, msg) = forker_channel.rx.recv().await.expect("mesh alive");

        let Message::Unit(ev) = msg else {
            continue;
        };

        match ev.unit.round {
            0 => {
                harvested.insert(ev.unit.creator, ev.unit.hash());
            }
            1 => {
                harvested.extend(
                    ev.unit
                        .parents
                        .iter()
                        .filter(|(creator, _)| **creator != forker)
                        .map(|(creator, hash)| (*creator, *hash)),
                );
            }
            _ => {}
        }
    }

    let (honest_x, honest_y) = {
        let mut it = harvested.into_iter();
        (
            it.next().expect("harvested two"),
            it.next().expect("harvested two"),
        )
    };

    let spoofed_parents: BTreeMap<PeerId, UnitHash> = BTreeMap::from([
        (forker, branch_a.unit.hash()),
        (honest_x.0, branch_b.unit.hash()),
        (honest_y.0, honest_y.1),
    ]);

    let creator_spoof = build_envelope(&keychains[&forker], 1, forker, spoofed_parents, 42);

    let skip_parents: BTreeMap<PeerId, UnitHash> =
        BTreeMap::from([(forker, branch_a.unit.hash()), honest_x, honest_y]);

    let round_skip = build_envelope(&keychains[&forker], 2, forker, skip_parents, 43);

    for recipient in [1u8, 2u8, 3u8] {
        forker_channel.deliver(
            PeerId::from(recipient),
            Message::Unit(creator_spoof.clone()),
        );
        forker_channel.deliver(PeerId::from(recipient), Message::Unit(round_skip.clone()));
    }

    let sequences = collect_sequences(engines.ordered_rxs).await;

    for h in engines.handles {
        h.abort();
    }

    let reference = assert_agreement(&sequences);

    for spoofed in [42u64, 43u64] {
        assert!(
            !reference.contains(&(forker, spoofed)),
            "a unit with spoofed parent positions must never be ordered",
        );
    }
}

/// Restarting an engine on its persisted unit table must reproduce the
/// exact live emission sequence — including under forks. Runs the
/// forking-peer scenario, then replays peer 1's engine on a clone of
/// its database behind a sink network and compares the streams under
/// the same `ROUND_LIMIT` cut.
#[tokio::test]
async fn replay_reproduces_order_under_forks() {
    let n = NumPeers::from(N_PEERS);
    let keychains = build_keychains(n);
    let mut channels = MockChannel::mesh(n, DROP_RATE);

    let forker = PeerId::from(0u8);
    let forker_channel = channels.remove(&forker).expect("mesh built above");

    let mut engines = spawn_engines(n, &keychains, channels);

    for (data, recipients) in [(1u64, vec![1u8, 2u8]), (2u64, vec![3u8])] {
        let signed = build_envelope(&keychains[&forker], 0, forker, BTreeMap::new(), data);

        for recipient in recipients {
            forker_channel.deliver(PeerId::from(recipient), Message::Unit(signed.clone()));
        }
    }

    let sequences = collect_sequences(engines.ordered_rxs).await;

    for h in engines.handles {
        h.abort();
    }

    assert_agreement(&sequences);

    let replayer = PeerId::from(1u8);

    let (ordered_tx, ordered_rx) = async_channel::unbounded();

    let engine = Engine::new(
        replayer,
        SESSION,
        n,
        engines.dbs.remove(&replayer).expect("spawned above"),
        keychains[&replayer].clone(),
        Arc::new(NullNetwork),
        TimestampDataProvider,
        ordered_tx,
        BftUnits,
    );

    let handle = tokio::spawn(engine.run());

    let mut replayed: Vec<(PeerId, u64)> = Vec::new();

    while let Ok(Ok((round, creator, datum))) =
        timeout(Duration::from_secs(2), ordered_rx.recv()).await
    {
        if round > ROUND_LIMIT {
            break;
        }
        replayed.push((creator, datum));
    }

    handle.abort();

    assert_eq!(
        replayed, sequences[&replayer],
        "replay must reproduce the live emission sequence",
    );
}
