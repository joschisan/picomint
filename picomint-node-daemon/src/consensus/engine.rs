use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{anyhow, ensure};
use async_channel::Receiver;
use bitcoin::hashes::sha256;
use futures::{StreamExt, stream};
use picomint_bft::{Engine as BftEngine, Keychain as BftKeychain, Round as BftRound};
use picomint_core::secp256k1::{SECP256K1, schnorr};
use picomint_core::session::{AcceptedItem, SessionOutcome, SessionState, session_header};
use picomint_core::tx::ConsensusItem;
use picomint_core::version::CONSENSUS_VERSION;
use picomint_core::{NodeId, NumNodesExt};
use picomint_redb::{DbRead, ReadTx, WriteTx};
use rand::seq::IteratorRandom;
use tracing::{info, instrument};

use crate::config::NodeConfig;
use crate::consensus::bft::{DataProvider, Network};
use crate::consensus::db::{
    AcceptedItemTable, AcceptedTxidTable, BftUnitDataTable, BftUnitSignatureTable, BftUnitTable,
    BlockHeightVoteTable, ConsensusVersionVoteTable, ResumeIndexTable, SessionSignaturesTable,
    consensus_block_height, consensus_version,
};
use crate::consensus::onchain;
use crate::consensus::server::Server;
use crate::p2p::{P2PMessage, Recipient, ReconnectP2PConnections};

/// BFT rounds a session runs for, which is what sets how long one lasts.
///
/// Not agreed at DKG: a mint that wants a different session length is a
/// mint running a different binary, and every node runs the same one by
/// construction.
const ROUNDS_PER_SESSION: u32 = 1000;

/// Bytes of accepted items a session collects before it closes.
///
/// A session outcome reaches a lagging node as a single p2p message, so
/// it has to stay inside `MAX_P2P_MESSAGE_SIZE`; a session that outgrew that
/// would be one no node could ever recover. Nothing else bounds what a busy
/// session collects, since the round cap only bounds an idle one.
///
/// Unlike the unit fill target this one is consensus: every node has to
/// cut the session at the same item, which is why it counts accepted items in
/// delivery order — the same items in the same order on every node — and
/// resumes the count from the database after a restart. Each item counts at
/// its full [`AcceptedItem`] encoding, node id included, so the tally is the
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
    // We need four nodes to run the atomic broadcast
    assert!(server.cfg.consensus.nodes.to_num_nodes().total() >= 4);

    loop {
        let session = get_finished_session_count(&server.db.begin_read());

        info!(session, "Starting consensus session");

        if run_session(&server, &connections, &submission_rx, session)
            .await
            .is_none()
        {
            return Ok(());
        }

        info!(session, "Completed consensus session");
    }
}

async fn run_session(
    server: &Server,
    connections: &ReconnectP2PConnections,
    submission_rx: &Receiver<ConsensusItem>,
    session: u32,
) -> Option<()> {
    // The bft engine creates units unpaced but work-gated: as fast as
    // new parents arrive while items await ordering, not at all while
    // idle. The session stops ordering items once it reaches
    // [`ROUNDS_PER_SESSION`] rounds (see [`order_items_until_cut`]),
    // which on a quiet mint can take arbitrarily long in wall
    // clock.

    // Both of these are filled straight from the p2p reader, so leaving
    // them unbounded would let a node turn its bandwidth into our memory —
    // the more so for signatures, which nothing reads until the session
    // cuts. Dropping when full costs nothing: a node rebroadcasts its
    // signature every second, and a node that holds the signed outcome
    // sends it again the next time we ask for the session.
    let num_nodes = server.cfg.consensus.nodes.to_num_nodes();

    let (outcomes_tx, outcomes_rx) = async_channel::bounded(num_nodes.total());
    let (signatures_tx, signatures_rx) = async_channel::bounded(num_nodes.total());

    let (ordered_tx, ordered_rx) = async_channel::unbounded();

    let network = Network::new(
        connections.clone(),
        outcomes_tx,
        signatures_tx,
        server.db.clone(),
    );

    let bft_engine = BftEngine::new(
        server.cfg.private.identity,
        session,
        num_nodes,
        server.db.clone(),
        build_keychain(&server.cfg),
        network,
        DataProvider::new(submission_rx.clone()),
        ordered_tx,
        BftUnitTable,
        BftUnitDataTable,
        BftUnitSignatureTable,
    );

    let bft_handle = tokio::spawn(bft_engine.run());

    // A validated threshold-signed outcome from a node supersedes local
    // participation in any phase, so the two race for the whole session.
    // Cancelling participation at an await inside item processing has to be
    // equivalent to crashing there: the dropped WriteTx rolls back, and
    // [`finalize_session`] re-applies the item from the adopted outcome.
    // That holds because module processing keeps no state outside the
    // WriteTx and its only external effect, the onchain module's tx broadcast,
    // tolerates replay — new module code has to preserve both properties.
    let close = tokio::select! {
        outcome = adopt_session(server, connections, session, outcomes_rx) => {
            SessionClose::Adopted(outcome?)
        }
        signatures = participate_in_session(
            server,
            connections,
            session,
            ordered_rx,
            signatures_rx,
        ) => SessionClose::Signed(signatures?),
    };

    info!(session, "Terminating BFT session");

    // The engine has no internal stopping condition, and it has to be dead
    // before [`finalize_session`] clears BFT_UNITS underneath it; abort it
    // now that we hold the signed outcome — nodes that still need it will
    // fetch via SessionIndex/SessionOutcome.
    bft_handle.abort();
    bft_handle.await.ok();

    finalize_session(server, session, close).await;

    Some(())
}

/// How the running session closed: with a threshold of signatures over
/// the header we folded ourselves, or with a signed outcome adopted from
/// a node that got there first.
enum SessionClose {
    Signed(BTreeMap<NodeId, schnorr::Signature>),
    Adopted(SessionOutcome),
}

/// Obtains the signed session outcome without ordering a single item, by
/// asking a random node for it at a fixed interval (3s prod / 300ms regtest)
/// until a validated one arrives. This is how a node that fell behind —
/// by crashing mid-session or by missing entire sessions — catches back up.
async fn adopt_session(
    server: &Server,
    connections: &ReconnectP2PConnections,
    session: u32,
    outcomes_rx: Receiver<(NodeId, SessionOutcome)>,
) -> Option<SessionOutcome> {
    let request_interval = if server.integration_test {
        Duration::from_millis(300)
    } else {
        Duration::from_secs(3)
    };

    let mut request_interval = tokio::time::interval(request_interval);

    loop {
        tokio::select! {
            result = outcomes_rx.recv() => {
                let (node, outcome) = result.ok()?;

                if validate_session_outcome(&server.cfg, session, &outcome) {
                    info!(session, %node, "Adopted signed session outcome from node");

                    return Some(outcome);
                }
            }
            _ = request_interval.tick() => {
                connections.send(
                    Recipient::Node(random_node(&server.cfg)),
                    P2PMessage::SessionIndex(session),
                );
            }
        }
    }
}

/// Takes part in the session: orders items until the session cut, then
/// signs the header folded on the way and collects a threshold of node
/// signatures over it.
async fn participate_in_session(
    server: &Server,
    connections: &ReconnectP2PConnections,
    session: u32,
    ordered_rx: Receiver<(BftRound, NodeId, ConsensusItem)>,
    signatures_rx: Receiver<(NodeId, schnorr::Signature)>,
) -> Option<BTreeMap<NodeId, schnorr::Signature>> {
    let header = order_items_until_cut(server, session, ordered_rx).await?;

    collect_threshold_signatures(server, connections, session, header, signatures_rx).await
}

/// Processes bft deliveries one committed write transaction at a time until
/// the session cut — the byte target or the round cap, whichever comes
/// first — and returns the header folded over the accepted items. Each
/// accepted item lands in ACCEPTED_ITEM under the session and its dense
/// position, with RESUME_INDEX advanced past it; a rejection rolls back
/// and leaves no trace, so a restart processes it again and rejects it
/// again.
async fn order_items_until_cut(
    server: &Server,
    session: u32,
    ordered_rx: Receiver<(BftRound, NodeId, ConsensusItem)>,
) -> Option<sha256::Hash> {
    // On crash replay bft re-emits from position 0, so we resume past
    // every position the prior run processed. The header and the byte
    // budget resume with it: a session that cut at a different item than
    // its nodes is one they never sign together.
    let resume_from = server
        .db
        .begin_read()
        .get(&ResumeIndexTable, &())
        .unwrap_or(0);

    let mut state = SessionState::new(session);

    server
        .db
        .begin_read()
        .prefix(&AcceptedItemTable, &session, |r| {
            r.for_each(|entry| state.fold(&entry.1));
        });

    let mut deliveries = Box::pin(stream::iter(0u64..).zip(ordered_rx));

    loop {
        // Ahead of the next delivery rather than after the last one: a run
        // that crashed between crossing the target and closing the session
        // comes back with the count already past it, and has to cut where
        // its nodes did rather than one item further on.
        if state.bytes >= SESSION_OUTCOME_BYTE_TARGET {
            return Some(state.header);
        }

        let (index, (round, node, item)) = deliveries.next().await?;

        if index < resume_from {
            continue;
        }

        if round >= ROUNDS_PER_SESSION {
            return Some(state.header);
        }

        let dbtx = server.db.begin_write();

        dbtx.insert(&ResumeIndexTable, &(), &(index + 1));

        if process_consensus_item(server, &dbtx, node, item.clone()).is_err() {
            continue;
        }

        let item = AcceptedItem { node, item };

        dbtx.insert(&AcceptedItemTable, &(session, state.index), &item);

        dbtx.commit();

        state.fold(&item);
    }
}

/// Signs the session header and rebroadcasts our signature every second
/// until a threshold of validated node signatures over it has arrived.
async fn collect_threshold_signatures(
    server: &Server,
    connections: &ReconnectP2PConnections,
    session: u32,
    header: sha256::Hash,
    signatures_rx: Receiver<(NodeId, schnorr::Signature)>,
) -> Option<BTreeMap<NodeId, schnorr::Signature>> {
    info!(session, "Signing session header...");

    let keychain = build_keychain(&server.cfg);

    let our_signature = keychain.sign(session, &header);

    // Send before counting: the last nodes to reach the cut usually find a
    // threshold of signatures already buffered and leave the loop below
    // before its first tick, so without this the faster nodes never see
    // their signature and fall back to adopting the outcome.
    connections.send(
        Recipient::Everyone,
        P2PMessage::SessionSignature(our_signature),
    );

    let mut signatures = BTreeMap::from_iter([(server.cfg.private.identity, our_signature)]);

    let mut broadcast_interval = tokio::time::interval(Duration::from_secs(1));

    while signatures.len() < server.cfg.consensus.nodes.to_num_nodes().threshold() {
        tokio::select! {
            result = signatures_rx.recv() => {
                let (node, signature) = result.ok()?;

                if keychain.verify(session, &header, &signature, node) {
                    signatures.insert(node, signature);

                    info!(session, %node, "Collected signature from node via P2P");
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

    info!(session, "Successfully collected threshold of signatures");

    Some(signatures)
}

/// Returns a random node ID excluding ourselves
fn random_node(cfg: &NodeConfig) -> NodeId {
    cfg.consensus
        .nodes
        .to_num_nodes()
        .node_ids()
        .filter(|p| *p != cfg.private.identity)
        .choose(&mut rand::thread_rng())
        .expect("We have at least four nodes")
}

/// Validate a SessionOutcome received via P2P
fn validate_session_outcome(cfg: &NodeConfig, session: u32, outcome: &SessionOutcome) -> bool {
    if outcome.signatures.len() != cfg.consensus.nodes.to_num_nodes().threshold() {
        return false;
    }

    let header = session_header(session, &outcome.items);

    let keychain = build_keychain(cfg);

    outcome
        .signatures
        .iter()
        .all(|(signer_id, sig)| keychain.verify(session, &header, sig, *signer_id))
}

/// Closes the session in a single write transaction: store the signatures,
/// clear the bft units and the delivery position for the next
/// session — and, for an adopted outcome, first process and store the
/// suffix of items we had not ordered ourselves. The atomicity is what
/// makes adoption crash-safe: either the whole session landed or none of
/// it did.
///
/// Determinism of item processing guarantees the items we accepted ourselves
/// form a prefix of an adopted outcome; anything else is a consensus failure.
async fn finalize_session(server: &Server, session: u32, close: SessionClose) {
    info!(session, "Finalizing session...");

    let dbtx = server.db.begin_write();

    let signatures = match close {
        SessionClose::Signed(signatures) => signatures,
        SessionClose::Adopted(outcome) => {
            let accepted = dbtx.prefix(&AcceptedItemTable, &session, |r| {
                r.map(|entry| entry.1).collect::<Vec<AcceptedItem>>()
            });

            let (processed, unprocessed) = outcome
                .items
                .split_at_checked(accepted.len())
                .expect("Consensus Failure: we accepted more items than mint consensus");

            assert!(
                accepted == processed,
                "Consensus Failure: our accepted items disagree with mint consensus"
            );

            info!(
                session,
                processed = processed.len(),
                unprocessed = unprocessed.len(),
                "Applying adopted session outcome"
            );

            for (index, item) in (accepted.len() as u64..).zip(unprocessed) {
                process_consensus_item(server, &dbtx, item.node, item.item.clone())
                    .expect("Rejected item accepted by mint consensus");

                dbtx.insert(&AcceptedItemTable, &(session, index), item);
            }

            outcome.signatures
        }
    };

    dbtx.insert_new(&SessionSignaturesTable, &session, &signatures);

    dbtx.clear_table(&ResumeIndexTable);

    dbtx.clear_table(&BftUnitTable);

    dbtx.clear_table(&BftUnitDataTable);

    dbtx.clear_table(&BftUnitSignatureTable);

    dbtx.commit();

    // Rejections only land during ordering, and a waiting submission RPC
    // has either read its entry by now or resubmits on the session close
    // this commit signals — so clearing here is what bounds the map.
    server.rejected.send_modify(|rejected| rejected.clear());
}

#[instrument(skip(server, dbtx, item), level = "info")]
fn process_consensus_item(
    server: &Server,
    dbtx: &WriteTx,
    node: NodeId,
    item: ConsensusItem,
) -> anyhow::Result<()> {
    match &item {
        ConsensusItem::Tx(tx) => {
            let txid = tx.compute_txid();

            ensure!(
                dbtx.get(&AcceptedTxidTable, &txid).is_none(),
                "Transaction is already accepted"
            );

            if let Err(error) = server.process_tx(dbtx, tx) {
                // Only our own submission has a submission RPC waiting on
                // it, and copies of an already accepted transaction bail at
                // the check above - so every rejection we record is final
                // and has a caller to fail.
                if node == server.cfg.private.identity {
                    server.rejected.send_modify(|rejected| {
                        rejected.insert(txid, error.clone());
                    });
                }

                return Err(anyhow!(error.to_string()));
            }

            dbtx.insert(&AcceptedTxidTable, &txid, &());
        }
        ConsensusItem::Module(ci) => {
            server.process_module_ci(dbtx, node, ci)?;
        }
        ConsensusItem::BlockHeight(vote) => {
            let old_block_height = consensus_block_height(server, dbtx);

            let current_vote = dbtx.insert(&BlockHeightVoteTable, &node, vote).unwrap_or(0);

            ensure!(current_vote < *vote, "Block height vote is redundant");

            let new_block_height = consensus_block_height(server, dbtx);

            assert!(old_block_height <= new_block_height);

            if new_block_height != old_block_height {
                info!(
                    %node,
                    vote,
                    old_block_height,
                    new_block_height,
                    "consensus block height advanced"
                );

                onchain::initialize_block_height(dbtx, old_block_height, new_block_height);
            }
        }
        ConsensusItem::Version(vote) => {
            let default_version = server.cfg.consensus.default_version;

            let current_vote = dbtx
                .insert(&ConsensusVersionVoteTable, &node, vote)
                .unwrap_or(default_version);

            ensure!(current_vote < *vote, "Consensus version vote is redundant");

            // A threshold has moved past what we know how to apply, so
            // every rule we would run from here on is the wrong one.
            // Halting is the only correct move left.
            assert!(
                consensus_version(server, dbtx) <= CONSENSUS_VERSION,
                "Node does not support the active consensus version, please upgrade"
            );
        }
    }

    Ok(())
}

pub fn get_finished_session_count(dbtx: &ReadTx) -> u32 {
    dbtx.iter_rev(&SessionSignaturesTable, |r| {
        r.next().map_or(0, |entry| entry.0 + 1)
    })
}

fn build_keychain(cfg: &NodeConfig) -> BftKeychain {
    let keypair = cfg.private.broadcast_secret_key.keypair(SECP256K1);

    let pubkeys = cfg
        .consensus
        .nodes
        .iter()
        .map(|(id, ep)| (*id, ep.broadcast_pk))
        .collect();

    BftKeychain::new(keypair, pubkeys)
}
