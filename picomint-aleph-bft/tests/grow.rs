//! Multi-peer mock test that grows a DAG across several rounds without any
//! networking layer — every peer's `Graph` and `Keychain` are driven directly
//! by the test harness, exercising insert / co-sign / confirm / next-round
//! triggering end-to-end.

use std::collections::BTreeMap;

use picomint_aleph_bft::{Graph, InsertOutcome, Keychain, Round, Unit};
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
    let mut graphs: Vec<Graph<u64>> = (0..N_PEERS).map(|_| Graph::new(n, SESSION)).collect();

    for round in 0..=ROUNDS {
        // Each creator builds and disseminates one unit at this round.
        for creator_idx in 0..N_PEERS {
            let creator = PeerId::from(creator_idx as u8);

            // Creator picks parents from its own view of the previous round
            // (empty for round 0).
            let parents = graphs[creator_idx]
                .parents_for(round, creator)
                .expect("parents available");

            let unit = Unit {
                session: SESSION,
                round,
                creator,
                parents,
                data: Vec::new(),
            };

            // Creator broadcasts the unit; every peer (including creator)
            // inserts it into their local graph.
            for graph in graphs.iter_mut() {
                let outcome = graph.insert_unit(unit.clone());
                assert_eq!(
                    outcome,
                    InsertOutcome::Accepted,
                    "round {round} creator {creator_idx}: insert failed",
                );
            }

            // Every peer (including creator) co-signs the unit and the sig
            // is gossiped to everyone. record_sig verifies and counts.
            let unit_hash = unit.hash();
            for signer_idx in 0..N_PEERS {
                let signer = PeerId::from(signer_idx as u8);
                let sig = keychains[signer_idx].sign(&unit_hash);
                for (verifier_idx, graph) in graphs.iter_mut().enumerate() {
                    let _ = graph.record_sig(round, creator, signer, sig, &keychains[verifier_idx]);
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

        // Every peer must now have all N units at this round confirmed.
        for (idx, graph) in graphs.iter().enumerate() {
            assert_eq!(
                graph.confirmed_count(round),
                N_PEERS,
                "peer {idx}: confirmed count at round {round} mismatch"
            );
        }
    }

    // Sanity: every peer's view of every confirmed slot agrees on hashes.
    for round in 0..=ROUNDS {
        for creator_idx in 0..N_PEERS {
            let creator = PeerId::from(creator_idx as u8);
            let hashes: Vec<_> = graphs
                .iter()
                .map(|g| g.entry(round, creator).expect("entry exists").hash())
                .collect();
            assert!(
                hashes.windows(2).all(|w| w[0] == w[1]),
                "peers diverge on (r={round}, c={creator_idx})"
            );
        }
    }
}
