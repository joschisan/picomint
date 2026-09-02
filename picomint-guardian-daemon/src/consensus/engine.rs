use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{anyhow, ensure};
use async_channel::Receiver;
use futures::{StreamExt, stream};
use picomint_bft::{Engine as BftEngine, INetwork, Keychain as BftKeychain, Round as BftRound};
use picomint_core::secp256k1::{SECP256K1, schnorr};
use picomint_core::session::{AcceptedItem, SessionOutcome, SignedSessionOutcome};
use picomint_core::tx::ConsensusItem;
use picomint_core::version::CONSENSUS_VERSION;
use picomint_core::{NumPeersExt, PeerId};
use picomint_encoding::Encodable;
use picomint_redb::{DbRead, ReadTx, WriteTx};
use rand::seq::IteratorRandom;
use tracing::{info, instrument};

use crate::config::ServerConfig;
use crate::consensus::bft::{DataProvider, Network};
use crate::consensus::db::{
    AcceptedItemTable, AcceptedTxTable, BftUnitsTable, BlockCountVoteTable,
    ConsensusVersionVoteTable, SignedSessionOutcomeTable, consensus_block_count, consensus_version,
};
use crate::consensus::server::Server;
use crate::consensus::wallet;
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
/// resumes the count from the database after a restart. Each item counts at
/// its full [`AcceptedItem`] encoding, peer id included, so the tally is the
/// wire size of the outcome's item list. The cut overshoots by the item that
/// crossed it, itself bounded by the transaction caps.
const SESSION_OUTCOME_BYTE_TARGET: usize = 1_000_000;

/// Runs the main server consensus loop.
#[instrument(name = "run", skip_all, fields(id=%server.cfg.private.identity))]
pub async fn run(
    server: Server,
    connections: ReconnectP2PConnections,
    submission_rx: Receiver<ConsensusItem>,
) -> anyhow::Result<()> {
    // We need four peers to run the atomic broadcast
    assert!(server.cfg.consensus.peers.to_num_peers().total() >= 4);

    loop {
        let session_index = get_finished_session_count(&server.db.begin_read());

        info!(session_index, "Starting consensus session");

        if run_session(&server, &connections, &submission_rx, session_index)
            .await
            .is_none()
        {
            return Ok(());
        }

        info!(session_index, "Completed consensus session");
    }
}

async fn run_session(
    server: &Server,
    connections: &ReconnectP2PConnections,
    submission_rx: &Receiver<ConsensusItem>,
    session_index: u32,
) -> Option<()> {
    // The bft engine creates units unpaced but work-gated: as fast as
    // new parents arrive while items await ordering, not at all while
    // idle. The session stops ordering items once it reaches
    // [`rounds_per_session`] rounds (see [`order_items_until_cut`]),
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

    // A validated threshold-signed outcome from a peer supersedes local
    // participation in any phase, so the two race for the whole session.
    // Cancelling participation at an await inside item processing has to be
    // equivalent to crashing there: the dropped WriteTx rolls back, and
    // [`finalize_session`] re-applies the item from the adopted outcome.
    // That holds because module processing keeps no state outside the
    // WriteTx and its only external effect, the wallet's tx broadcast,
    // tolerates replay — new module code has to preserve both properties.
    let signed_session_outcome = tokio::select! {
        outcome = adopt_session(server, connections, session_index, outcomes_rx) => outcome?,
        outcome = participate_in_session(
            server,
            connections,
            session_index,
            ordered_rx,
            signatures_rx,
        ) => outcome?,
    };

    assert!(
        validate_signed_session_outcome(&server.cfg, session_index, &signed_session_outcome),
        "Our created signed session outcome fails validation"
    );

    info!(session_index, "Terminating BFT session");

    // The engine has no internal stopping condition, and it has to be dead
    // before [`finalize_session`] clears BFT_UNITS underneath it; abort it
    // now that we hold the signed outcome — peers that still need it will
    // fetch via SessionIndex/SignedSessionOutcome.
    bft_handle.abort();
    bft_handle.await.ok();

    finalize_session(server, session_index, signed_session_outcome).await;

    Some(())
}

/// Obtains the signed session outcome without ordering a single item, by
/// asking a random peer for it at a fixed interval (3s prod / 300ms regtest)
/// until a validated one arrives. This is how a guardian that fell behind —
/// by crashing mid-session or by missing entire sessions — catches back up.
async fn adopt_session(
    server: &Server,
    connections: &ReconnectP2PConnections,
    session_index: u32,
    outcomes_rx: Receiver<(PeerId, SignedSessionOutcome)>,
) -> Option<SignedSessionOutcome> {
    let request_interval = if server.cfg.consensus.network == bitcoin::Network::Regtest {
        Duration::from_millis(300)
    } else {
        Duration::from_secs(3)
    };

    let mut request_interval = tokio::time::interval(request_interval);

    loop {
        tokio::select! {
            result = outcomes_rx.recv() => {
                let (peer, outcome) = result.ok()?;

                if validate_signed_session_outcome(&server.cfg, session_index, &outcome) {
                    info!(session_index, %peer, "Adopted signed session outcome from peer");

                    return Some(outcome);
                }
            }
            _ = request_interval.tick() => {
                connections.send(
                    Recipient::Peer(random_peer(&server.cfg)),
                    P2PMessage::SessionIndex(session_index),
                );
            }
        }
    }
}

/// Obtains the signed session outcome by taking part in the session: order
/// items until the session cut, then sign the resulting outcome and collect
/// a threshold of peer signatures over it.
async fn participate_in_session(
    server: &Server,
    connections: &ReconnectP2PConnections,
    session_index: u32,
    ordered_rx: Receiver<(BftRound, PeerId, ConsensusItem)>,
    signatures_rx: Receiver<(PeerId, schnorr::Signature)>,
) -> Option<SignedSessionOutcome> {
    order_items_until_cut(server, ordered_rx).await?;

    let session_outcome = SessionOutcome {
        items: pending_accepted_items(server),
    };

    collect_threshold_signatures(
        server,
        connections,
        session_index,
        session_outcome,
        signatures_rx,
    )
    .await
}

/// Processes bft deliveries one committed write transaction at a time until
/// the session cut — the byte target or the round cap, whichever comes
/// first. Accepted items land in ACCEPTED_ITEM under their
/// delivery position; rejected ones leave no trace beyond their position
/// being skipped.
async fn order_items_until_cut(
    server: &Server,
    ordered_rx: Receiver<(BftRound, PeerId, ConsensusItem)>,
) -> Option<()> {
    // We enumerate every bft delivery for this session; ACCEPTED_ITEM is
    // sparse (rejected positions are absent). On crash replay bft re-emits
    // from position 0, so we resume past the highest position already in
    // the table — every position up to and including it was already
    // processed (accepted *or* rejected) by the prior run.
    let resume_from = server
        .db
        .begin_read()
        .iter_rev(&AcceptedItemTable, |r| r.next().map(|entry| entry.0))
        .map_or(0, |k| k + 1);

    // The byte budget resumes where the prior run left it for the same
    // reason: a session that cut at a different item than its peers is one
    // they never sign together.
    let mut n_bytes: usize = server.db.begin_read().iter(&AcceptedItemTable, |r| {
        r.map(|entry| entry.1.consensus_encode_to_vec().len()).sum()
    });

    let mut deliveries = Box::pin(stream::iter(0u64..).zip(ordered_rx));

    loop {
        // Ahead of the next delivery rather than after the last one: a run
        // that crashed between crossing the target and closing the session
        // comes back with the count already past it, and has to cut where
        // its peers did rather than one item further on.
        if n_bytes >= SESSION_OUTCOME_BYTE_TARGET {
            return Some(());
        }

        let (index, (round, peer, item)) = deliveries.next().await?;

        if index < resume_from {
            continue;
        }

        if round >= rounds_per_session(&server.cfg) {
            return Some(());
        }

        let dbtx = server.db.begin_write();

        if process_consensus_item(server, &dbtx, peer, item.clone())
            .await
            .is_ok()
        {
            let accepted_item = AcceptedItem { peer, item };

            dbtx.insert(&AcceptedItemTable, &index, &accepted_item);

            dbtx.commit();

            n_bytes += accepted_item.consensus_encode_to_vec().len();
        }
    }
}

/// Signs the session header and rebroadcasts our signature every second
/// until a threshold of validated peer signatures over it has arrived.
async fn collect_threshold_signatures(
    server: &Server,
    connections: &ReconnectP2PConnections,
    session_index: u32,
    session_outcome: SessionOutcome,
    signatures_rx: Receiver<(PeerId, schnorr::Signature)>,
) -> Option<SignedSessionOutcome> {
    let header = session_outcome.header(session_index);

    info!(session_index, "Signing session header...");

    let keychain = build_keychain(&server.cfg);

    let our_signature = keychain.sign(session_index, &header);

    let mut signatures = BTreeMap::from_iter([(server.cfg.private.identity, our_signature)]);

    let mut broadcast_interval = tokio::time::interval(Duration::from_secs(1));

    while signatures.len() < server.cfg.consensus.peers.to_num_peers().threshold() {
        tokio::select! {
            result = signatures_rx.recv() => {
                let (peer, signature) = result.ok()?;

                if keychain.verify(session_index, &header, &signature, peer) {
                    signatures.insert(peer, signature);

                    info!(session_index, %peer, "Collected signature from peer via P2P");
                }
            }
            _ = broadcast_interval.tick() => {
                connections.send(
                    Recipient::Everyone,
                    P2PMessage::SessionSignature(our_signature),
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
    session_index: u32,
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
        .iter(&AcceptedItemTable, |r| r.map(|entry| entry.1).collect())
}

/// Closes the session in a single write transaction: process whatever suffix
/// of the outcome we have not applied yet, clear the per-session tables and
/// store the signed outcome. The atomicity is what makes adoption crash-safe
/// — either the whole session landed or none of it did, so a restart always
/// finds ACCEPTED_ITEM agreeing with everything already applied.
///
/// Determinism of item processing guarantees the items we accepted ourselves
/// form a prefix of the signed outcome; anything else is a consensus failure.
async fn finalize_session(
    server: &Server,
    session_index: u32,
    signed_session_outcome: SignedSessionOutcome,
) {
    let pending_accepted_items = pending_accepted_items(server);

    assert!(
        pending_accepted_items.len() <= signed_session_outcome.session_outcome.items.len(),
        "Consensus Failure: we accepted more items than federation consensus"
    );

    let (processed, unprocessed) = signed_session_outcome
        .session_outcome
        .items
        .split_at(pending_accepted_items.len());

    assert!(
        processed.iter().eq(pending_accepted_items.iter()),
        "Consensus Failure: pending accepted items disagree with federation consensus"
    );

    info!(
        session_index,
        processed = processed.len(),
        unprocessed = unprocessed.len(),
        "Finalizing session..."
    );

    let dbtx = server.db.begin_write();

    for accepted_item in unprocessed {
        process_consensus_item(
            server,
            &dbtx,
            accepted_item.peer,
            accepted_item.item.clone(),
        )
        .await
        .expect("Rejected item accepted by federation consensus");
    }

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

    // Rejections only land during ordering, and a waiting submission RPC
    // has either read its entry by now or resubmits on the session close
    // this commit signals — so clearing here is what bounds the map.
    server.rejected.send_modify(|rejected| rejected.clear());
}

#[instrument(skip(server, dbtx, item), level = "info")]
async fn process_consensus_item(
    server: &Server,
    dbtx: &WriteTx,
    peer: PeerId,
    item: ConsensusItem,
) -> anyhow::Result<()> {
    match &item {
        ConsensusItem::Tx(tx) => {
            let txid = tx.compute_txid();

            ensure!(
                dbtx.get(&AcceptedTxTable, &txid).is_none(),
                "Transaction is already accepted"
            );

            if let Err(error) = server.process_tx(dbtx, tx) {
                // Only our own submission has a submission RPC waiting on
                // it, and copies of an already accepted transaction bail at
                // the check above - so every rejection we record is final
                // and has a caller to fail.
                if peer == server.cfg.private.identity {
                    server.rejected.send_modify(|rejected| {
                        rejected.insert(txid, error.clone());
                    });
                }

                return Err(anyhow!(error.to_string()));
            }

            dbtx.insert(&AcceptedTxTable, &txid, &());

            let audit = server.audit(dbtx);

            assert!(audit.total >= 0, "Failed audit: {audit:?}");
        }
        ConsensusItem::Module(ci) => {
            server.process_module_ci(dbtx, peer, ci).await?;
        }
        ConsensusItem::BlockCount(vote) => {
            let old_block_count = consensus_block_count(server, dbtx);

            let current_vote = dbtx.insert(&BlockCountVoteTable, &peer, vote).unwrap_or(0);

            ensure!(current_vote < *vote, "Block count vote is redundant");

            let new_block_count = consensus_block_count(server, dbtx);

            assert!(old_block_count <= new_block_count);

            if new_block_count != old_block_count {
                info!(
                    %peer,
                    vote,
                    old_block_count,
                    new_block_count,
                    "consensus block count advanced"
                );

                wallet::sync_blocks(server, dbtx, old_block_count, new_block_count).await;
            }
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
                consensus_version(server, dbtx) <= CONSENSUS_VERSION,
                "Guardian does not support the active consensus version, please upgrade"
            );
        }
    }

    Ok(())
}

pub fn get_finished_session_count(dbtx: &ReadTx) -> u32 {
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
