use crate::{
    testing::{init_log, spawn_honest_member, HonestMember},
    NumPeers, SpawnHandle,
};
use aleph_bft_mock::{DataProvider, Router, Spawner, UnreliableHook};
use futures::StreamExt;
use serial_test::serial;

async fn honest_members_agree_on_batches(
    n_members: NumPeers,
    n_alive: NumPeers,
    n_batches: usize,
    network_reliability: Option<f64>,
) {
    init_log();
    let spawner = Spawner::new();
    let mut exits = Vec::new();
    let mut handles = Vec::new();
    let mut batch_rxs = Vec::new();
    let (mut net_hub, networks) = Router::new(n_members);
    if let Some(reliability) = network_reliability {
        net_hub.add_hook(UnreliableHook::new(reliability));
    }
    spawner.spawn("network-hub", net_hub);

    for (network, _) in networks {
        let ix = network.index();
        if n_alive.peer_ids().any(|p| p == ix) {
            let HonestMember {
                finalization_rx,
                exit_tx,
                handle,
                ..
            } = spawn_honest_member(spawner, ix, n_members, vec![], DataProvider::new(), network);
            batch_rxs.push(finalization_rx);
            exits.push(exit_tx);
            handles.push(handle);
        }
    }

    let mut batches = vec![];
    for mut rx in batch_rxs.drain(..) {
        let mut batches_per_ix = vec![];
        for _ in 0..n_batches {
            let batch = rx.next().await.unwrap();
            batches_per_ix.push(batch);
        }
        batches.push(batches_per_ix);
    }

    for node_ix in n_alive.peer_ids().skip(1) {
        assert_eq!(batches[0], batches[node_ix.to_usize()]);
    }
    for exit in exits {
        let _ = exit.send(());
    }
    for handle in handles {
        let _ = handle.await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn small_honest_all_alive() {
    honest_members_agree_on_batches(NumPeers::from(4 as usize), 4.into(), 5, None).await;
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn small_honest_one_crash() {
    honest_members_agree_on_batches(NumPeers::from(4 as usize), 3.into(), 5, None).await;
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn small_honest_one_crash_unreliable_network() {
    honest_members_agree_on_batches(NumPeers::from(4 as usize), 3.into(), 5, Some(0.9)).await;
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn medium_honest_all_alive() {
    honest_members_agree_on_batches(NumPeers::from(31 as usize), 31.into(), 5, None).await;
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn medium_honest_ten_crashes() {
    honest_members_agree_on_batches(NumPeers::from(31 as usize), 21.into(), 5, None).await;
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn medium_honest_ten_crashes_unreliable_network() {
    honest_members_agree_on_batches(NumPeers::from(31 as usize), 21.into(), 5, Some(0.9)).await;
}
