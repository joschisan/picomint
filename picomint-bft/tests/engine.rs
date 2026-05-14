//! End-to-end test of the per-peer engine driving DAG growth, order
//! extraction, and emission of ordered items through the bft engine's
//! `ordered_tx` channel over a mock channel-based network. N engines
//! spawn as tasks, each owns a per-peer in-memory `Database`, a
//! `Keychain`, a `MockChannel`, and the receiving end of its own
//! `ordered_tx`. Each peer feeds its own per-unit payload of one
//! `u64` carrying a monotonic timestamp. The engines never stop on
//! their own — the test drains each peer's `ordered_rx` until
//! `ROUND_LIMIT` is reached, then aborts the engine tasks. We then
//! assert every peer observed the same total order of `(creator,
//! item)` pairs.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_channel::{Receiver, Sender};
use async_trait::async_trait;
use picomint_bft::{
    Cosig, DataProvider, Engine, INetwork, Keychain, Message, Recipient, Round, Unit,
};
use picomint_core::secp256k1::{Keypair, SECP256K1, rand};
use picomint_core::{NumPeers, PeerId};
use picomint_redb::{Database, table};
use rand::Rng;

table!(BftUnits, (Round, PeerId) => Unit<u64>, "bft-units");
table!(BftCosigs, (Round, PeerId, PeerId) => Cosig, "bft-cosigs");

/// Per-recipient probability of silently dropping a message in the mock
/// network. Each unicast send and each fan-out leg of a broadcast rolls
/// independently.
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
    senders: BTreeMap<PeerId, Sender<(PeerId, Message<u64>)>>,
    rx: Receiver<(PeerId, Message<u64>)>,
}

impl MockChannel {
    /// Build a fully-connected mesh of channels, one per peer in `n`.
    fn mesh(n: NumPeers) -> BTreeMap<PeerId, MockChannel> {
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
                    senders: senders.clone(),
                    rx,
                };
                (own_id, channel)
            })
            .collect()
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

                    if rand::random::<f64>() < DROP_RATE {
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

                if rand::random::<f64>() < DROP_RATE {
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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is past UNIX epoch")
        .as_millis() as u64
}

struct TimestampDataProvider;

#[async_trait]
impl DataProvider<u64> for TimestampDataProvider {
    async fn get_data(&mut self) -> Vec<u64> {
        vec![now_ms()]
    }
}

const N_PEERS: usize = 4;
const ROUND_LIMIT: Round = 100;
/// Same default as `picomint-guardian-daemon::config::BFT_ROUND_DELAY_MS`.
const UNIT_DELAY: Duration = Duration::from_millis(50);
const SESSION: u64 = 0;

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

#[tokio::test]
async fn engines_agree_on_ordered_data() {
    let n = NumPeers::from(N_PEERS);
    let mut keychains = build_keychains(n);
    let mut channels = MockChannel::mesh(n);

    let mut handles = Vec::new();
    let mut ordered_rxs: BTreeMap<PeerId, Receiver<(Round, PeerId, u64)>> = BTreeMap::new();

    for peer in n.peer_ids() {
        let db = Database::open_in_memory();
        let (ordered_tx, ordered_rx) = async_channel::unbounded();
        ordered_rxs.insert(peer, ordered_rx);

        let network = Arc::new(channels.remove(&peer).expect("mesh built above"));

        let engine = Engine::new(
            peer,
            SESSION,
            n,
            db,
            keychains.remove(&peer).expect("built above"),
            network,
            TimestampDataProvider,
            Box::new(|_round| UNIT_DELAY),
            ordered_tx,
            BftUnits,
            BftCosigs,
        );

        handles.push(tokio::spawn(engine.run()));
    }

    let mut reader_handles = Vec::new();
    for (peer, ordered_rx) in ordered_rxs {
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
    let mut delays_by_observer: BTreeMap<PeerId, Vec<u64>> = BTreeMap::new();
    for h in reader_handles {
        let (peer, seq, delays) = h.await.expect("reader task panicked");
        sequences.insert(peer, seq);
        delays_by_observer.insert(peer, delays);
    }

    for (peer, delays) in &delays_by_observer {
        let n_items = delays.len();
        let avg = delays.iter().sum::<u64>() as f64 / n_items as f64;
        let max = delays.iter().copied().max().unwrap_or(0);
        println!("peer {peer}: items={n_items} avg_delay={avg:.1}ms max_delay={max}ms");
    }

    for h in handles {
        h.abort();
    }

    let reference = sequences
        .values()
        .next()
        .expect("at least one peer")
        .clone();
    assert!(!reference.is_empty(), "expected at least one ordered item");

    for (peer, seq) in &sequences {
        assert_eq!(seq, &reference, "peer {peer} disagrees on total order");
    }
}
