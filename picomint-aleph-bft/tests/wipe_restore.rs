//! Regression test for the wipe-and-restore scenario.
//!
//! When a guardian's data dir is wiped and the daemon restarts, the
//! restored peer rejoins the federation mid-session. Peers may unicast
//! `Confirmed { unit, sigs }` for *this peer's own* prior round-0 unit
//! (via `handle_status`'s gap-fill path) before the engine has had a
//! chance to call `advance_round`. The engine must skip past that
//! pre-filled slot rather than try to build a duplicate unit at the
//! same `(round, creator)`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use picomint_aleph_bft::{
    Backup, DataProvider, DynNetwork, Graph, INetwork, InsertOutcome, Keychain, Message,
    NoopBackup, Recipient, Round, Unit, run,
};
use picomint_core::secp256k1::{Keypair, SECP256K1, rand};
use picomint_core::{NumPeers, PeerId};

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

struct EmptyDataProvider;

#[async_trait]
impl DataProvider<u64> for EmptyDataProvider {
    async fn get_data(&mut self) -> Vec<u64> {
        vec![]
    }
}

/// Network that drops every send and parks every receive. Lets the
/// engine run in isolation so the only thing that can move state is the
/// pre-populated graph passed into `run`.
struct SilentNetwork;

#[async_trait]
impl INetwork<Message<u64>> for SilentNetwork {
    fn send(&self, _: Recipient, _: Message<u64>) {}

    async fn receive(&self) -> Option<(PeerId, Message<u64>)> {
        std::future::pending().await
    }

    async fn receive_from_peer(&self, _: PeerId) -> Option<Message<u64>> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn engine_skips_pre_filled_own_slot_after_wipe_restore() {
    let n = NumPeers::from(4);
    let mut keychains = build_keychains(n);
    let session = 1u64;
    let own_id = PeerId::from(0u8);

    let unit = Unit::<u64> {
        session,
        round: 0,
        creator: own_id,
        parents: BTreeMap::new(),
        data: vec![],
    };

    let mut graph = Graph::<u64>::new(n, session);
    assert_eq!(graph.insert_unit(unit.clone()), InsertOutcome::Accepted);

    let verifier = keychains.get(&own_id).expect("built").clone();
    for signer in n.peer_ids().take(n.threshold()) {
        let kc = keychains.get(&signer).expect("built");
        let sig = kc.sign(&unit.hash());
        graph.record_sig(0, own_id, signer, sig, &verifier);
    }
    assert!(graph.is_confirmed(0, own_id));

    let own_keychain = keychains.remove(&own_id).expect("built");
    let backup: Arc<dyn Backup<u64>> = Arc::new(NoopBackup);
    let network: DynNetwork<Message<u64>> = Arc::new(SilentNetwork);
    let (tx, _rx) = async_channel::unbounded::<(Round, PeerId, u64)>();

    let handle = tokio::spawn(run(
        own_id,
        graph,
        own_keychain,
        network,
        backup,
        EmptyDataProvider,
        tx,
        Box::new(|_round| Duration::from_millis(50)),
    ));

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert!(
        !handle.is_finished(),
        "engine panicked when its own round-0 slot was pre-filled (wipe-restore P2P recovery path)",
    );

    handle.abort();
}
