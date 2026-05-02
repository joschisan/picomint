//! Multi-peer mock test that grows a DAG across several rounds without any
//! networking layer — every peer's `Graph` and `Keychain` are driven directly
//! by the test harness, exercising insert / co-sign / confirm / next-round
//! triggering end-to-end.

use std::collections::BTreeMap;
use std::sync::Arc;

use picomint_bft::{Graph, Keychain, NoopBackup, Round, Unit};
use picomint_core::PeerId;
use picomint_core::secp256k1::{Keypair, SECP256K1, rand};

const N_PEERS: usize = 4;
const ROUNDS: Round = 50;
const SESSION: u64 = 0;

fn build_peers() -> Vec<Keychain> {
    let keypairs: Vec<(PeerId, Keypair)> = (0..N_PEERS)
        .map(|i| {
            (
                PeerId::from(i as u8),
                Keypair::new(SECP256K1, &mut rand::thread_rng()),
            )
        })
        .collect();

    let pubkeys: BTreeMap<_, _> = keypairs
        .iter()
        .map(|(id, kp)| (*id, kp.x_only_public_key().0))
        .collect();

    keypairs
        .into_iter()
        .map(|(_, kp)| Keychain::new(kp, pubkeys.clone()))
        .collect()
}

#[test]
fn grow_dag_across_rounds() {
    let keychains = build_peers();
    let n = picomint_core::NumPeers::from(N_PEERS);
    let mut graphs: Vec<Graph<u64>> = (0..N_PEERS)
        .map(|_| {
            // Drop the receiver — this test inspects the graph
            // directly and doesn't observe the ordered stream.
            let (tx, _rx) = async_channel::unbounded();
            Graph::new(n, SESSION, Arc::new(NoopBackup), tx)
        })
        .collect();

    for round in 0..=ROUNDS {
        // Each creator builds and disseminates one unit at this round.
        for creator_idx in 0..N_PEERS {
            let creator = PeerId::from(creator_idx as u8);

            // Creator picks parents from its own view of the previous round
            // (empty for round 0).
            let parents = graphs[creator_idx]
                .parents_for(round)
                .expect("parents available");

            let unit = Unit {
                session: SESSION,
                round,
                creator,
                parents,
                data: Vec::new(),
            };

            // Creator broadcasts the unit with its own sig; every peer
            // (including creator) inserts it into their local graph.
            let creator_sig = keychains[creator_idx].sign(&unit);
            for (verifier_idx, graph) in graphs.iter_mut().enumerate() {
                assert!(
                    graph
                        .insert_unit(
                            unit.clone(),
                            creator_sig,
                            BTreeMap::new(),
                            &keychains[verifier_idx],
                        )
                        .is_some(),
                    "round {round} creator {creator_idx}: insert failed at verifier {verifier_idx}",
                );
            }

            // Every other peer cosigns the unit and the cosig is
            // gossiped to everyone via record_cosig.
            for signer_idx in 0..N_PEERS {
                if signer_idx == creator_idx {
                    continue;
                }
                let signer = PeerId::from(signer_idx as u8);
                let sig = keychains[signer_idx].sign(&unit);
                for (verifier_idx, graph) in graphs.iter_mut().enumerate() {
                    let _ =
                        graph.record_cosig(round, creator, signer, sig, &keychains[verifier_idx]);
                }
            }

            // After threshold sigs gossiped, the unit must be confirmed
            // in every peer's graph.
            for (idx, graph) in graphs.iter().enumerate() {
                assert!(
                    graph.is_confirmed(round, creator),
                    "peer {idx}: unit (r={round}, c={creator_idx}) not confirmed",
                );
            }
        }
    }

    // Sanity: every peer's view of every confirmed slot agrees on the
    // unit body (sigs may differ in collection order across peers).
    for round in 0..=ROUNDS {
        for creator_idx in 0..N_PEERS {
            let creator = PeerId::from(creator_idx as u8);
            let units: Vec<_> = graphs
                .iter()
                .map(|g| {
                    g.entry(round, creator)
                        .expect("entry exists")
                        .unit()
                        .clone()
                })
                .collect();
            assert!(
                units.windows(2).all(|w| w[0] == w[1]),
                "peers diverge on (r={round}, c={creator_idx})"
            );
        }
    }
}
