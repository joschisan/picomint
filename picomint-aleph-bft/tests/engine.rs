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
use std::sync::{Arc, Once};
use std::time::Duration;

use async_trait::async_trait;
use picomint_aleph_bft::{
    Backup, DataProvider, Graph, INetwork, Keychain, MockChannel, NoopBackup, Round, run,
};
use picomint_core::secp256k1::{Keypair, SECP256K1, rand};
use picomint_core::{NumPeers, PeerId};

/// Hands out a strictly-monotonic sequence of `u64`s, one per call.
struct CounterDataProvider {
    counter: u64,
}

#[async_trait]
impl DataProvider<u64> for CounterDataProvider {
    async fn get_data(&mut self) -> Vec<u64> {
        let datum = self.counter;
        self.counter += 1;
        vec![datum]
    }
}

static INIT_TRACING: Once = Once::new();

fn init_tracing() {
    INIT_TRACING.call_once(|| {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    });
}

const N_PEERS: usize = 4;
const ROUND_LIMIT: Round = 100;
/// Same default as `picomint-server-daemon::config::ALEPH_ROUND_DELAY_MS`.
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
    init_tracing();

    let n = NumPeers::from(N_PEERS);
    let mut keychains = build_keychains(n);
    let mut channels = MockChannel::mesh(n);

    let mut handles = Vec::new();
    let mut ordered_rxs = BTreeMap::new();

    for (idx, peer_id) in n.peer_ids().enumerate() {
        let (tx, rx) = async_channel::unbounded::<(Round, PeerId, u64)>();
        ordered_rxs.insert(peer_id, rx);

        let data_provider = CounterDataProvider {
            counter: idx as u64 * 10_000,
        };

        let network = channels
            .remove(&peer_id)
            .expect("mesh built above")
            .into_dyn();

        let backup: Arc<dyn Backup<u64>> = Arc::new(NoopBackup);

        let h = tokio::spawn(run(
            peer_id,
            Graph::new(n, SESSION),
            keychains.remove(&peer_id).expect("built above"),
            network,
            backup,
            data_provider,
            tx,
            Box::new(|_round| UNIT_DELAY),
        ));

        handles.push(h);
    }

    let mut sequences: BTreeMap<PeerId, Vec<(PeerId, u64)>> = BTreeMap::new();
    for (peer_id, rx) in ordered_rxs {
        let mut seq = Vec::new();
        while let Ok((round, creator, datum)) = rx.recv().await {
            if round > ROUND_LIMIT {
                break;
            }
            seq.push((creator, datum));
        }
        sequences.insert(peer_id, seq);
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
