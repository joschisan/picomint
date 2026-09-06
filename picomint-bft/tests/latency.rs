//! Latency simulation over the mock mesh: every item is handed to all
//! four engines at once, the way a tx submission fans out to every node,
//! and the run reports how long the threshold-th node takes to emit it
//! and how many rounds the committing head trails the earliest unit
//! that carried the item. Run with
//! `cargo test -p picomint-bft --test latency -- --ignored --nocapture`.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender};
use async_trait::async_trait;
use picomint_bft::{
    DataProvider, Engine, INetwork, Keychain, Message, Recipient, Round, UnitEnvelope, UnitHash,
};
use picomint_core::secp256k1::{Keypair, SECP256K1, rand};
use picomint_core::{NodeId, NumNodes};
use picomint_redb::{Database, table};
use rand::Rng;
use tokio::time::{sleep, timeout};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing::{Event, Instrument, Subscriber, info_span};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

table!(BftUnits, UnitHash => UnitEnvelope<u64>, "bft-units");

const N_NODES: usize = 4;
const SESSION: u32 = 0;
const N_ITEMS: u64 = 400;

struct Profile {
    name: &'static str,
    base: Duration,
    jitter: Duration,
    interval: Duration,
}

const PROFILES: [Profile; 3] = [
    Profile {
        name: "loopback 1ms",
        base: Duration::from_millis(1),
        jitter: Duration::ZERO,
        interval: Duration::from_millis(60),
    },
    Profile {
        name: "metro 10ms +-3",
        base: Duration::from_millis(10),
        jitter: Duration::from_millis(3),
        interval: Duration::from_millis(100),
    },
    Profile {
        name: "residential 30ms +-10",
        base: Duration::from_millis(30),
        jitter: Duration::from_millis(10),
        interval: Duration::from_millis(200),
    },
];

struct MockChannel {
    own_id: NodeId,
    base: Duration,
    jitter: Duration,
    senders: BTreeMap<NodeId, Sender<(NodeId, Message<u64>)>>,
    rx: Receiver<(NodeId, Message<u64>)>,
}

impl MockChannel {
    fn mesh(n: NumNodes, base: Duration, jitter: Duration) -> BTreeMap<NodeId, MockChannel> {
        let mut receivers = BTreeMap::new();
        let mut senders = BTreeMap::new();

        for node in n.node_ids() {
            let (tx, rx) = async_channel::unbounded();
            senders.insert(node, tx);
            receivers.insert(node, rx);
        }

        n.node_ids()
            .map(|own_id| {
                let rx = receivers.remove(&own_id).expect("inserted above");
                let channel = MockChannel {
                    own_id,
                    base,
                    jitter,
                    senders: senders.clone(),
                    rx,
                };
                (own_id, channel)
            })
            .collect()
    }

    fn delayed_send(&self, to: NodeId, msg: Message<u64>) {
        let sender = self.senders[&to].clone();
        let from = self.own_id;
        let jitter = rand::thread_rng().gen_range(0..=self.jitter.as_micros() as u64);
        let delay = self.base + Duration::from_micros(jitter);

        tokio::spawn(async move {
            sleep(delay).await;
            let _ = sender.send((from, msg)).await;
        });
    }
}

impl INetwork<u64> for MockChannel {
    fn send(&self, recipient: Recipient, msg: Message<u64>) {
        match recipient {
            Recipient::Everyone => {
                for to in self.senders.keys() {
                    if *to != self.own_id {
                        self.delayed_send(*to, msg.clone());
                    }
                }
            }
            Recipient::Node(to) => self.delayed_send(to, msg),
        }
    }

    async fn receive(&self) -> Option<(NodeId, Message<u64>)> {
        self.rx.recv().await.ok()
    }
}

/// The daemon's submission-channel provider: items arrive on a channel,
/// `get_data` drains whatever is queued, and the pull instant of every
/// item is recorded so the unit that carried it can be found in the
/// engine's `created unit` trace.
struct ChannelDataProvider {
    rx: Receiver<u64>,
    pending: VecDeque<u64>,
    pulls: Arc<Mutex<BTreeMap<u64, Instant>>>,
}

#[async_trait]
impl DataProvider<u64> for ChannelDataProvider {
    fn get_data(&mut self) -> Vec<u64> {
        let mut items: Vec<u64> = self.pending.drain(..).collect();

        while let Ok(item) = self.rx.try_recv() {
            items.push(item);
        }

        let now = Instant::now();

        self.pulls
            .lock()
            .expect("no poisoning")
            .extend(items.iter().map(|item| (*item, now)));

        items
    }

    async fn wait_for_data(&mut self) {
        if let Ok(item) = self.rx.recv().await {
            self.pending.push_back(item);
        }
    }
}

#[derive(Clone, Copy)]
struct NodeTag(u8);

/// Per node, the instants and rounds of every `created unit` and
/// `elected head` event the engine logged.
#[derive(Default)]
struct Trace {
    created: BTreeMap<u8, Vec<(Instant, Round)>>,
    /// Per node, the head round that emitted each `(round, creator)` unit.
    emitted: BTreeMap<u8, BTreeMap<(Round, NodeId), Round>>,
}

#[derive(Default)]
struct TraceLayer {
    trace: Arc<Mutex<Trace>>,
}

#[derive(Default)]
struct FieldVisitor {
    id: Option<u64>,
    round: Option<u64>,
    head_round: Option<u64>,
    creator: Option<u8>,
    message: String,
}

impl Visit for FieldVisitor {
    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "id" => self.id = Some(value),
            "round" => self.round = Some(value),
            "head_round" => self.head_round = Some(value),
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = format!("{value:?}"),
            "creator" => self.creator = format!("{value:?}").parse().ok(),
            _ => {}
        }
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for TraceLayer {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut visitor = FieldVisitor::default();

        attrs.record(&mut visitor);

        if let (Some(node), Some(span)) = (visitor.id, ctx.span(id)) {
            span.extensions_mut().insert(NodeTag(node as u8));
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut visitor = FieldVisitor::default();

        event.record(&mut visitor);

        let Some(round) = visitor.round else {
            return;
        };

        let Some(node) = ctx.event_span(event).and_then(|span| {
            span.scope()
                .find_map(|s| s.extensions().get::<NodeTag>().copied())
        }) else {
            return;
        };

        let mut trace = self.trace.lock().expect("no poisoning");

        match visitor.message.as_str() {
            "created unit" => trace
                .created
                .entry(node.0)
                .or_default()
                .push((Instant::now(), round as Round)),
            "emitted unit" => {
                if let (Some(creator), Some(head)) = (visitor.creator, visitor.head_round) {
                    trace
                        .emitted
                        .entry(node.0)
                        .or_default()
                        .insert((round as Round, NodeId::from(creator)), head as Round);
                }
            }
            _ => {}
        }
    }
}

fn build_keychains(n: NumNodes) -> BTreeMap<NodeId, Keychain> {
    let keypairs: BTreeMap<NodeId, Keypair> = n
        .node_ids()
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

fn pct(sorted: &[f64], q: f64) -> f64 {
    sorted[((sorted.len() as f64 * q) as usize).min(sorted.len() - 1)]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// The round of the first event at or after `at`.
fn round_at(events: &[(Instant, Round)], at: Instant) -> Option<Round> {
    events.iter().find(|e| e.0 >= at).map(|e| e.1)
}

async fn run_profile(profile: &Profile, trace: Arc<Mutex<Trace>>) {
    let n = NumNodes::from(N_NODES);
    let keychains = build_keychains(n);
    let channels = MockChannel::mesh(n, profile.base, profile.jitter);

    let mut handles = Vec::new();
    let mut submitters = BTreeMap::new();
    let mut pulls = BTreeMap::new();
    let mut readers = Vec::new();

    for (node, channel) in channels {
        let (submit_tx, submit_rx) = async_channel::unbounded();
        let (ordered_tx, ordered_rx) = async_channel::unbounded();
        let node_pulls = Arc::new(Mutex::new(BTreeMap::new()));

        submitters.insert(node, submit_tx);
        pulls.insert(node, node_pulls.clone());

        let provider = ChannelDataProvider {
            rx: submit_rx,
            pending: VecDeque::new(),
            pulls: node_pulls,
        };

        let engine = Engine::new(
            node,
            SESSION,
            n,
            Database::open_in_memory(),
            keychains[&node].clone(),
            channel,
            provider,
            ordered_tx,
            BftUnits,
        );

        let span = info_span!("node", id = node.to_usize() as u64);

        handles.push(tokio::spawn(engine.run().instrument(span)));

        readers.push(tokio::spawn(async move {
            let mut received: BTreeMap<u64, (Instant, Round, NodeId)> = BTreeMap::new();

            while received.len() < N_ITEMS as usize {
                let Ok((round, creator, item)) = ordered_rx.recv().await else {
                    break;
                };

                // Every unit that carries the item emits it once; the
                // daemon keeps the first delivery and rejects the rest.
                received
                    .entry(item)
                    .or_insert((Instant::now(), round, creator));
            }

            (node, received)
        }));
    }

    let mut injected: BTreeMap<u64, Instant> = BTreeMap::new();

    for item in 0..N_ITEMS {
        injected.insert(item, Instant::now());

        for submitter in submitters.values() {
            submitter.send(item).await.expect("engine alive");
        }

        sleep(profile.interval).await;
    }

    let deadline = Duration::from_secs(30);

    let mut received: BTreeMap<NodeId, BTreeMap<u64, (Instant, Round, NodeId)>> = BTreeMap::new();

    for reader in readers {
        let (node, items) = timeout(deadline, reader)
            .await
            .expect("every node emits every item")
            .expect("reader task panicked");

        received.insert(node, items);
    }

    for handle in handles {
        handle.abort();
    }

    let trace = trace.lock().expect("no poisoning");

    let node0 = NodeId::from(0);

    let mut accept = Vec::new();
    let mut spread = BTreeMap::new();
    let mut lag_latency: BTreeMap<Round, Vec<f64>> = BTreeMap::new();
    let mut joint: BTreeMap<(Round, Round), usize> = BTreeMap::new();

    for (item, t0) in &injected {
        let mut arrivals: Vec<Duration> = received
            .values()
            .filter_map(|items| items.get(item))
            .map(|entry| entry.0.duration_since(*t0))
            .collect();

        if arrivals.len() < N_NODES {
            continue;
        }

        arrivals.sort();

        let carry: Vec<Round> = n
            .node_ids()
            .filter_map(|node| {
                let pulled = *pulls[&node].lock().expect("no poisoning").get(item)?;

                round_at(trace.created.get(&(node.to_usize() as u8))?, pulled)
            })
            .collect();

        let delivered = received[&node0][item];

        let Some(head) = trace.emitted[&0].get(&(delivered.1, delivered.2)).copied() else {
            continue;
        };

        let (Some(min), Some(max)) = (carry.iter().min(), carry.iter().max()) else {
            continue;
        };

        let latency = ms(arrivals[n.threshold() - 1]);

        accept.push(latency);
        *spread.entry(max - min).or_insert(0) += 1;
        *joint
            .entry((max - min, head.saturating_sub(*min)))
            .or_insert(0) += 1;
        lag_latency
            .entry(head.saturating_sub(*min))
            .or_default()
            .push(latency);
    }

    accept.sort_by(f64::total_cmp);

    println!(
        "\n== {} : {} items, threshold-of-{} accept latency",
        profile.name,
        accept.len(),
        n.threshold()
    );
    println!(
        "   p50 {:5.1}  p90 {:5.1}  p99 {:5.1} ms",
        pct(&accept, 0.5),
        pct(&accept, 0.9),
        pct(&accept, 0.99)
    );
    println!("   carry-round spread across nodes: {spread:?}");
    println!("   (spread, lag) -> n: {joint:?}");
    println!("   lag rounds |    n | p50 ms | p90 ms");

    for (lag, mut latencies) in lag_latency {
        latencies.sort_by(f64::total_cmp);

        println!(
            "   {lag:10} | {:4} | {:6.1} | {:6.1}",
            latencies.len(),
            pct(&latencies, 0.5),
            pct(&latencies, 0.9)
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "prints a latency report; run explicitly with --ignored --nocapture"]
async fn head_lag_report() {
    let layer = TraceLayer::default();
    let trace = layer.trace.clone();

    tracing_subscriber::registry().with(layer).init();

    for profile in &PROFILES {
        trace.lock().expect("no poisoning").created.clear();
        trace.lock().expect("no poisoning").emitted.clear();

        run_profile(profile, trace.clone()).await;
    }
}
