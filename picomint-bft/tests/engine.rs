//! End-to-end test of the per-peer engine driving DAG growth, order
//! extraction, and `(round, creator, datum)` emission over a mock
//! channel-based network. N engines spawn as tasks, each owns a `Graph`,
//! `Keychain`, `MockChannel`, and (inside `run`) an `Extender`. Each peer
//! feeds its own per-unit `Vec<u64>` payload via a closure. The engines
//! never stop on their own — the test reads each peer's stream up to and
//! including the last item at `ROUND_LIMIT`, then aborts the engine
//! tasks. We then assert every peer observed the same total order of
//! `(creator, datum)` pairs.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use picomint_bft::{
    Backup, DataProvider, Graph, INetwork, Keychain, MockChannel, NoopBackup, Round, run,
};
use picomint_core::secp256k1::{Keypair, SECP256K1, rand};
use picomint_core::{NumPeers, PeerId};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is past UNIX epoch")
        .as_millis() as u64
}

/// Stamps each datum with the current unix timestamp (ms) at the moment
/// the engine asks for it.
struct TimestampDataProvider;

#[async_trait]
impl DataProvider<u64> for TimestampDataProvider {
    async fn get_data(&mut self) -> Vec<u64> {
        vec![now_ms()]
    }
}

const N_PEERS: usize = 4;
const ROUND_LIMIT: Round = 100;
/// Same default as `picomint-server-daemon::config::BFT_ROUND_DELAY_MS`.
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
    let mut ordered_rxs = BTreeMap::new();

    for peer_id in n.peer_ids() {
        let (tx, rx) = async_channel::unbounded::<(Round, PeerId, u64)>();
        ordered_rxs.insert(peer_id, rx);

        let network = channels
            .remove(&peer_id)
            .expect("mesh built above")
            .into_dyn();

        let backup: Arc<dyn Backup<u64>> = Arc::new(NoopBackup);

        let graph = Graph::new(n, SESSION, backup, tx);

        let h = tokio::spawn(run(
            peer_id,
            graph,
            keychains.remove(&peer_id).expect("built above"),
            network,
            TimestampDataProvider,
            Box::new(|_round| UNIT_DELAY),
        ));

        handles.push(h);
    }

    let mut reader_handles = Vec::new();
    for (peer_id, rx) in ordered_rxs {
        reader_handles.push(tokio::spawn(async move {
            let mut seq = Vec::new();
            let mut delays = Vec::new();
            while let Ok((round, creator, datum)) = rx.recv().await {
                if round > ROUND_LIMIT {
                    break;
                }
                delays.push(now_ms().saturating_sub(datum));
                seq.push((creator, datum));
            }
            (peer_id, seq, delays)
        }));
    }

    let mut sequences: BTreeMap<PeerId, Vec<(PeerId, u64)>> = BTreeMap::new();
    let mut delays_by_observer: BTreeMap<PeerId, Vec<u64>> = BTreeMap::new();
    for h in reader_handles {
        let (peer_id, seq, delays) = h.await.expect("reader task panicked");
        sequences.insert(peer_id, seq);
        delays_by_observer.insert(peer_id, delays);
    }

    for (peer_id, delays) in &delays_by_observer {
        let n_items = delays.len();
        let avg = delays.iter().sum::<u64>() as f64 / n_items as f64;
        let max = delays.iter().copied().max().unwrap_or(0);
        println!("peer {peer_id}: items={n_items} avg_delay={avg:.1}ms max_delay={max}ms");
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

    for (peer_id, seq) in &sequences {
        assert_eq!(seq, &reference, "peer {peer_id} disagrees on total order");
    }
}
