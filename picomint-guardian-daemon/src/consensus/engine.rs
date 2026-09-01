use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{anyhow, ensure};
use async_channel::Receiver;
use futures::StreamExt;
use picomint_bft::{Engine as BftEngine, INetwork, Keychain as BftKeychain, Round as BftRound};
use picomint_core::secp256k1::{SECP256K1, schnorr};
use picomint_core::session::{AcceptedItem, SessionOutcome, SignedSessionOutcome};
use picomint_core::tx::{ConsensusItem, TxError};
use picomint_core::version::CONSENSUS_VERSION;
use picomint_core::{NumPeersExt, PeerId, TransactionId};
use picomint_encoding::Encodable;
use picomint_redb::{DbRead, ReadTx, WriteTx};
use rand::seq::IteratorRandom;
use tokio::sync::broadcast;
use tracing::{info, instrument};

use crate::config::ServerConfig;
use crate::consensus::bft::{DataProvider, Network};
use crate::consensus::db::{
    AcceptedItemTable, AcceptedTxTable, BftUnitsTable, ConsensusVersionVoteTable,
    SignedSessionOutcomeTable, consensus_version,
};
use crate::consensus::server::Server;
use crate::p2p::{P2PMessage, Recipient, ReconnectP2PConnections};

/// BFT rounds a session runs for, which is what sets how long one lasts.
///
/// Follows from the network rather than being agreed at DKG: every guardian
/// on a federation is on the same network by construction, so the two can
/// never disagree, and a federation that wants shorter sessions is a
/// federation running a different binary.
fn rounds_per_session(cfg: &ServerConfig) -> u32 {
    if cfg.consensus.network == bitcoin::Network::Regtest {
        100
    } else {
        10000
    }
}

/// Bytes of accepted items a session collects before it closes.
///
/// A session outcome reaches a lagging guardian as a single p2p message, so
/// it has to stay inside `MAX_P2P_MESSAGE_SIZE`; a session that outgrew that
/// would be one no peer could ever recover. Nothing else bounds what a busy
/// session collects, since the round cap only bounds an idle one.
///
/// Unlike the unit fill target this one is consensus: every guardian has to
/// cut the session at the same item, which is why it counts accepted items in
/// delivery order — the same items in the same order on every peer — and
/// resumes the count from the database after a restart. The cut overshoots by
/// the item that crossed it, itself bounded by the transaction caps.
const SESSION_OUTCOME_BYTE_TARGET: usize = 1_000_000;

/// Most accepted items a session may collect before it closes.
///
/// The byte budget alone would let a session of items too small to spend it
/// run to near a hundred thousand entries, each of which a recovering guardian
/// walks and each of which carries the index and peer it was filed under. A
/// count is what bounds that, and it is a cut on the same terms as the byte
/// budget: read back from the database on restart, checked before the next
/// delivery. A session lands on it exactly rather than overshooting, since an
/// item moves the count by one.
///
/// Under load the byte budget cuts first, so what this sets is the cadence of
/// an idle federation — the block count votes two modules cast per peer per
/// block are all such a session collects, which is a session every few days.
const SESSION_OUTCOME_ITEM_LIMIT: usize = 10_000;

/// Runs the main server consensus loop.
#[instrument(name = "run", skip_all, fields(id=%server.cfg.private.identity))]
pub async fn run(
    server: Server,
    connections: ReconnectP2PConnections,
    submission_rx: Receiver<ConsensusItem>,
    tx_reject_tx: broadcast::Sender<(TransactionId, TxError)>,
) -> anyhow::Result<()> {
    // We need four peers to run the atomic broadcast
    assert!(server.cfg.consensus.peers.to_num_peers().total() >= 4);

    loop {
        let session_index = get_finished_session_count(&server.db.begin_read());

        info!(session_index, "Starting consensus session");

        if run_session(
            &server,
            &connections,
            &submission_rx,
            &tx_reject_tx,
            session_index,
        )
        .await
        .is_none()
        {
            return Ok(());
        }

        info!(?session_index, "Completed consensus session");
    }
}

async fn run_session(
    server: &Server,
    connections: &ReconnectP2PConnections,
    submission_rx: &Receiver<ConsensusItem>,
    tx_reject_tx: &broadcast::Sender<(TransactionId, TxError)>,
    session_index: u64,
) -> Option<()> {
    // The bft engine creates units unpaced but work-gated: as fast as
    // new parents arrive while items await ordering, not at all while
    // idle. The session stops ordering items once it reaches
    // [`rounds_per_session`] rounds (see the ordering loop below),
    // which on a quiet federation can take arbitrarily long in wall
    // clock.

    // Both of these are filled straight from the p2p reader, so leaving
    // them unbounded would let a peer turn its bandwidth into our memory —
    // the more so for signatures, which nothing reads until the session
    // cuts. Dropping when full costs nothing: a peer rebroadcasts its
    // signature every second, and a peer that holds the signed outcome
    // sends it again the next time we ask for the session.
    let num_peers = server.cfg.consensus.peers.to_num_peers();

    let (outcomes_tx, outcomes_rx) = async_channel::bounded(num_peers.total());
    let (signatures_tx, signatures_rx) = async_channel::bounded(num_peers.total());

    let (ordered_tx, ordered_rx) = async_channel::unbounded();

    let network = Network::new(
        connections.clone(),
        outcomes_tx,
        signatures_tx,
        server.db.clone(),
    )
    .into_dyn();

    let bft_engine = BftEngine::new(
        server.cfg.private.identity,
        session_index,
        num_peers,
        server.db.clone(),
        build_keychain(&server.cfg),
        network,
        DataProvider::new(submission_rx.clone()),
        ordered_tx,
        BftUnitsTable,
    );

    let bft_handle = tokio::spawn(bft_engine.run());

    let signed_session_outcome = complete_signed_session_outcome(
        server,
        connections,
        tx_reject_tx,
        session_index,
        outcomes_rx,
        signatures_rx,
        ordered_rx,
    )
    .await?;

    assert!(
        validate_signed_session_outcome(&server.cfg, session_index, &signed_session_outcome),
        "Our created signed session outcome fails validation"
    );

    info!(?session_index, "Terminating BFT session");

    // The engine has no internal stopping condition; abort it now that
    // we hold the signed outcome — peers that still need it will fetch
    // via SessionIndex/SignedSessionOutcome.
    bft_handle.abort();
    bft_handle.await.ok();

    complete_session(server, session_index, signed_session_outcome);

    Some(())
}

async fn complete_signed_session_outcome(
    server: &Server,
    connections: &ReconnectP2PConnections,
    tx_reject_tx: &broadcast::Sender<(TransactionId, TxError)>,
    session_index: u64,
    outcomes_rx: Receiver<(PeerId, SignedSessionOutcome)>,
    signatures_rx: Receiver<(PeerId, schnorr::Signature)>,
    ordered_rx: Receiver<(BftRound, PeerId, ConsensusItem)>,
) -> Option<SignedSessionOutcome> {
    // We request the signed session outcome from a random peer at a fixed
    // interval (3s prod / 300ms regtest).
    let broadcast_interval = if server.cfg.consensus.network == bitcoin::Network::Regtest {
        Duration::from_millis(300)
    } else {
        Duration::from_secs(3)
    };
    let mut index_broadcast_interval = tokio::time::interval(broadcast_interval);

    // We enumerate every bft delivery for this session; ACCEPTED_ITEM
    // is sparse (rejected positions are absent). On crash replay bft
    // re-emits from position 0, so we resume past the highest index
    // already in AcceptedItemTable — every position up to and
    // including it was already processed (accepted *or* rejected) by
    // the prior run.
    let resume_from = server
        .db
        .begin_read()
        .iter_rev(&AcceptedItemTable, |r| r.next().map(|entry| entry.0))
        .map_or(0, |k| k as usize + 1);

    // The byte budget resumes where the prior run left it for the same
    // reason: a session that cut at a different item than its peers is one
    // they never sign together.
    let mut n_bytes: usize = server.db.begin_read().iter(&AcceptedItemTable, |r| {
        r.map(|(_, accepted)| accepted.item.consensus_encode_to_vec().len())
            .sum()
    });

    // As does the item count, which cuts a session of items too small for
    // the byte budget to reach.
    let mut n_items: usize = server
        .db
        .begin_read()
        .iter(&AcceptedItemTable, |r| r.count());

    let mut ordered_rx = Box::pin(ordered_rx.enumerate());

    // We build a session outcome out of the ordered batches until either we
    // have processed a session's worth of rounds, collected a session's
    // worth of items or bytes, or a threshold signed session outcome is
    // obtained from our peers
    loop {
        // Ahead of the next delivery rather than after the last one: a run
        // that crashed between crossing the target and closing the session
        // comes back with the count already past it, and has to cut where
        // its peers did rather than one item further on.
        if n_bytes >= SESSION_OUTCOME_BYTE_TARGET || n_items >= SESSION_OUTCOME_ITEM_LIMIT {
            break;
        }

        tokio::select! {
            result = ordered_rx.next() => {
                let (index, (round, creator, item)) = result?;

                if index < resume_from {
                    continue;
                }

                if round >= rounds_per_session(&server.cfg) {
                    break;
                }

                let dbtx = server.db.begin_write();

                if process_consensus_item(server, tx_reject_tx, &dbtx, index as u64, creator, &item).await.is_ok() {
                    dbtx.commit();

                    n_bytes += item.consensus_encode_to_vec().len();

                    n_items += 1;
                }
            },
            result = outcomes_rx.recv() => {
                let (peer, p2p_outcome) = result.ok()?;

                // Validate signatures
                if validate_signed_session_outcome(&server.cfg, session_index, &p2p_outcome) {
                    info!(
                        session_index,
                        peer = %peer,
                        "Received SignedSessionOutcome via P2P while collection signatures"
                    );

                    let pending_accepted_items = pending_accepted_items(server);

                    // this panics if we have more accepted items than the signed session outcome
                    let (processed, unprocessed) = p2p_outcome
                        .session_outcome
                        .items
                        .split_at(pending_accepted_items.len());

                    info!(
                        ?session_index,
                        processed = %processed.len(),
                        unprocessed = %unprocessed.len(),
                        "Processing remaining items..."
                    );

                    assert!(
                        processed.iter().eq(pending_accepted_items.iter()),
                        "Consensus Failure: pending accepted items disagree with federation consensus"
                    );

                    let dbtx = server.db.begin_write();

                    for accepted_item in unprocessed {
                        process_consensus_item(
                            server,
                            tx_reject_tx,
                            &dbtx,
                            accepted_item.index,
                            accepted_item.peer,
                            &accepted_item.item,
                        )
                        .await
                        .expect("Rejected item accepted by federation consensus");
                    }

                    dbtx.commit();

                    info!(
                        ?session_index,
                        peer = %peer,
                        "Successfully recovered session via P2P"
                    );

                    return Some(p2p_outcome);
                }
            }
            _ = index_broadcast_interval.tick() => {
                connections.send(
                    Recipient::Peer(random_peer(&server.cfg)),
                    P2PMessage::SessionIndex(session_index),
                );
            }
        }
    }

    let items = pending_accepted_items(server);

    let session_outcome = SessionOutcome { items };

    let header = session_outcome.header(session_index);

    info!(?session_index, "Signing session header...");

    let keychain = build_keychain(&server.cfg);

    let our_signature = keychain.sign(session_index, &header);

    let mut signatures = BTreeMap::from_iter([(server.cfg.private.identity, our_signature)]);

    // We request the session signature every second to all peers
    let mut signature_broadcast_interval = tokio::time::interval(Duration::from_secs(1));

    // We collect the ordered signatures until we either obtain a threshold
    // signature or a signed session outcome arrives from our peers
    while signatures.len() < server.cfg.consensus.peers.to_num_peers().threshold() {
        tokio::select! {
            result = signatures_rx.recv() => {
                let (peer, signature) = result.ok()?;

                if keychain.verify(session_index, &header, &signature, peer) {
                    signatures.insert(peer, signature);

                    info!(
                        session_index,
                        peer = %peer,
                        "Collected signature from peer via P2P"
                    );
                }

            }
            result = outcomes_rx.recv() => {
                let (peer, p2p_outcome) = result.ok()?;

                if validate_signed_session_outcome(&server.cfg, session_index, &p2p_outcome) {
                    assert_eq!(
                        header,
                        p2p_outcome.session_outcome.header(session_index),
                        "Consensus Failure: header disagrees with federation consensus"
                    );

                    info!(
                        session_index,
                        %peer,
                        "Recovered session via P2P while collecting signatures"
                    );

                    return Some(p2p_outcome);
                }
            }
            _ = signature_broadcast_interval.tick() => {
                connections.send(
                    Recipient::Everyone,
                    P2PMessage::SessionSignature(our_signature),
                );
            }
            _ = index_broadcast_interval.tick() => {
                connections.send(
                    Recipient::Peer(random_peer(&server.cfg)),
                    P2PMessage::SessionIndex(session_index),
                );
            }
        }
    }

    info!(
        session_index,
        "Successfully collected threshold of signatures"
    );

    Some(SignedSessionOutcome {
        session_outcome,
        signatures,
    })
}

/// Returns a random peer ID excluding ourselves
fn random_peer(cfg: &ServerConfig) -> PeerId {
    cfg.consensus
        .peers
        .to_num_peers()
        .peer_ids()
        .filter(|p| *p != cfg.private.identity)
        .choose(&mut rand::thread_rng())
        .expect("We have at least three peers")
}

/// Validate a SignedSessionOutcome received via P2P
fn validate_signed_session_outcome(
    cfg: &ServerConfig,
    session_index: u64,
    outcome: &SignedSessionOutcome,
) -> bool {
    if outcome.signatures.len() != cfg.consensus.peers.to_num_peers().threshold() {
        return false;
    }

    let header = outcome.session_outcome.header(session_index);

    let keychain = build_keychain(cfg);

    outcome
        .signatures
        .iter()
        .all(|(signer_id, sig)| keychain.verify(session_index, &header, sig, *signer_id))
}

fn pending_accepted_items(server: &Server) -> Vec<AcceptedItem> {
    server
        .db
        .begin_read()
        .iter(&AcceptedItemTable, |r| r.map(|(_, item)| item).collect())
}

fn complete_session(
    server: &Server,
    session_index: u64,
    signed_session_outcome: SignedSessionOutcome,
) {
    let dbtx = server.db.begin_write();

    dbtx.clear_table(&AcceptedItemTable);

    dbtx.clear_table(&BftUnitsTable);

    assert!(
        dbtx.insert(
            &SignedSessionOutcomeTable,
            &session_index,
            &signed_session_outcome,
        )
        .is_none(),
        "We tried to overwrite a signed session outcome"
    );

    dbtx.commit();
}

#[instrument(skip(server, tx_reject_tx, dbtx, item), level = "info")]
async fn process_consensus_item(
    server: &Server,
    tx_reject_tx: &broadcast::Sender<(TransactionId, TxError)>,
    dbtx: &WriteTx,
    index: u64,
    peer: PeerId,
    item: &ConsensusItem,
) -> anyhow::Result<()> {
    match item {
        ConsensusItem::Module(ci) => {
            server.process_module_ci(dbtx, peer, ci).await?;
        }
        ConsensusItem::Tx(tx) => {
            let txid = tx.compute_txid();

            ensure!(
                dbtx.get(&AcceptedTxTable, &txid).is_none(),
                "Transaction is already accepted"
            );

            if let Err(error) = server.process_tx(dbtx, tx) {
                // Only our own submission has a submission RPC waiting on
                // it, and copies of an already accepted transaction bail at
                // the check above - so every rejection we broadcast is
                // final and has a caller to fail.
                if peer == server.cfg.private.identity {
                    tx_reject_tx.send((txid, error.clone())).ok();
                }

                return Err(anyhow!(error.to_string()));
            }

            dbtx.insert(&AcceptedTxTable, &txid, &());

            let audit = server.audit(dbtx);

            assert!(audit.total >= 0, "Failed audit: {audit:?}");
        }
        ConsensusItem::Version(vote) => {
            let default_version = server.cfg.consensus.default_version;

            let current_vote = dbtx
                .insert(&ConsensusVersionVoteTable, &peer, vote)
                .unwrap_or(default_version);

            ensure!(current_vote < *vote, "Consensus version vote is redundant");

            // A threshold has moved past what we know how to apply, so
            // every rule we would run from here on is the wrong one.
            // Halting is the only correct move left.
            assert!(
                consensus_version(
                    dbtx,
                    server.cfg.consensus.peers.to_num_peers(),
                    default_version
                ) <= CONSENSUS_VERSION,
                "Guardian does not support the active consensus version, please upgrade"
            );
        }
    }

    dbtx.insert(
        &AcceptedItemTable,
        &index,
        &AcceptedItem {
            index,
            peer,
            item: item.clone(),
        },
    );

    Ok(())
}

pub fn get_finished_session_count(dbtx: &ReadTx) -> u64 {
    dbtx.iter_rev(&SignedSessionOutcomeTable, |r| {
        r.next().map_or(0, |entry| entry.0 + 1)
    })
}

fn build_keychain(cfg: &ServerConfig) -> BftKeychain {
    let keypair = cfg.private.broadcast_secret_key.keypair(SECP256K1);

    let pubkeys = cfg
        .consensus
        .peers
        .iter()
        .map(|(id, ep)| (*id, ep.broadcast_pk))
        .collect();

    BftKeychain::new(keypair, pubkeys)
}
