use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail};
use async_channel::Receiver;
use bitcoin::hashes::sha256;
use picomint_aleph_bft::{
    Graph as AlephGraph, INetwork, Keychain as AlephKeychain, Round as AlephRound, run as run_aleph,
};
use picomint_core::secp256k1::{SECP256K1, schnorr};
use picomint_core::session_outcome::{AcceptedItem, SessionOutcome, SignedSessionOutcome};
use picomint_core::task::{TaskGroup, TaskHandle};
use picomint_core::transaction::ConsensusItem;
use picomint_core::{NumPeers, NumPeersExt, PeerId};
use picomint_encoding::Encodable;
use picomint_redb::{Database, ReadTransaction, WriteTransaction};
use rand::Rng;
use rand::seq::IteratorRandom;
use tokio::sync::watch;
use tokio::time::sleep;
use tracing::{debug, info, instrument, trace};

use crate::LOG_CONSENSUS;
use crate::config::ServerConfig;
use crate::consensus::aleph::{DataProvider, Network, RedbBackup};
use crate::consensus::db::{
    ACCEPTED_ITEM, ACCEPTED_TRANSACTION, ALEPH_UNITS, SIGNED_SESSION_OUTCOME,
};
use crate::consensus::server::process_transaction_with_server;
use crate::p2p::{P2PMessage, Recipient, ReconnectP2PConnections};

/// Runs the main server consensus loop
pub struct ConsensusEngine {
    pub server: crate::consensus::server::Server,
    pub db: Database,
    pub cfg: ServerConfig,
    pub submission_receiver: Receiver<ConsensusItem>,
    pub shutdown_receiver: watch::Receiver<Option<u64>>,
    pub connections: ReconnectP2PConnections<P2PMessage>,
    pub ci_status_senders: BTreeMap<PeerId, watch::Sender<Option<u64>>>,
    pub task_group: TaskGroup,
}

impl ConsensusEngine {
    fn num_peers(&self) -> NumPeers {
        self.cfg.consensus.peers.to_num_peers()
    }

    fn identity(&self) -> PeerId {
        self.cfg.private.identity
    }

    #[instrument(target = LOG_CONSENSUS, name = "run", skip_all, fields(id=%self.cfg.private.identity))]
    pub async fn run(self) -> anyhow::Result<()> {
        self.run_consensus(self.task_group.make_handle()).await
    }

    pub async fn run_consensus(&self, task_handle: TaskHandle) -> anyhow::Result<()> {
        // We need four peers to run the atomic broadcast
        assert!(self.num_peers().total() >= 4);

        while !task_handle.is_shutting_down() {
            let session_index = self.get_finished_session_count().await;

            info!(
                target: LOG_CONSENSUS,
                session_index,
                "Starting consensus session"
            );

            if self
                .run_session(self.connections.clone(), session_index)
                .await
                .is_none()
            {
                return Ok(());
            }

            info!(target: LOG_CONSENSUS, ?session_index, "Completed consensus session");

            if Some(session_index) == self.shutdown_receiver.borrow().to_owned() {
                info!(target: LOG_CONSENSUS, "Initiating shutdown, waiting for peers to complete the session...");

                sleep(Duration::from_mins(1)).await;

                break;
            }
        }

        info!(target: LOG_CONSENSUS, "Consensus task shut down");

        Ok(())
    }

    pub async fn run_session(
        &self,
        connections: ReconnectP2PConnections<P2PMessage>,
        session_index: u64,
    ) -> Option<()> {
        // In order to bound a sessions RAM consumption we need to bound its number of
        // units and therefore its number of rounds. Since we use a session to
        // create a naive secp256k1 threshold signature for the header of session
        // outcome we have to guarantee that an attacker cannot exhaust our
        // memory by preventing the creation of a threshold signature, thereby
        // keeping the session open indefinitely. Hence, after a certain round
        // index, we increase the delay between rounds exponentially such that
        // the end of the aleph bft session would only be reached after a minimum
        // of 10 years. In case of such an attack the broadcast stops ordering any
        // items until the attack subsides as no items are ordered while the
        // signatures are collected. The maximum RAM consumption of the aleph bft
        // broadcast instance is therefore bound by:
        //
        // self.keychain.peer_count()
        //      * (broadcast_rounds_per_session + 1000)
        //      * ALEPH_BFT_UNIT_BYTE_LIMIT

        /// AlephBFT round delay (ms). Byzantine-fault-only; the ordering floor is
        /// dominated by network latency in practice.
        const ROUND_DELAY_MS: f64 = 50.0;
        const BASE: f64 = 1.02;

        let rounds_per_session = self.cfg.consensus.aleph_rounds_per_session;

        let unit_delay = Box::new(move |round_index: u16| {
            let delay = if round_index == 0 {
                0.0
            } else {
                ROUND_DELAY_MS
                    * BASE.powf(round_index.saturating_sub(rounds_per_session) as f64)
                    * rand::thread_rng().gen_range(0.5..=1.5)
            };

            Duration::from_millis(delay.round() as u64)
        });

        let (ordered_sender, ordered_receiver) =
            async_channel::unbounded::<(AlephRound, PeerId, ConsensusItem)>();
        let (signed_outcomes_sender, signed_outcomes_receiver) = async_channel::unbounded();
        let (signatures_sender, signatures_receiver) = async_channel::unbounded();

        let network = Network::new(
            connections.clone(),
            signed_outcomes_sender,
            signatures_sender,
            self.db.clone(),
        )
        .into_dyn();

        let backup = Arc::new(RedbBackup::new(self.db.clone()));

        let aleph_handle = tokio::spawn(run_aleph(
            self.identity(),
            AlephGraph::new(self.num_peers(), session_index),
            build_keychain(&self.cfg),
            network,
            backup,
            DataProvider::new(self.submission_receiver.clone()),
            ordered_sender,
            unit_delay,
        ));

        let signed_session_outcome = self
            .complete_signed_session_outcome(
                session_index,
                ordered_receiver,
                signed_outcomes_receiver,
                signatures_receiver,
                connections,
            )
            .await?;

        assert!(
            self.validate_signed_session_outcome(&signed_session_outcome, session_index),
            "Our created signed session outcome fails validation"
        );

        info!(target: LOG_CONSENSUS, ?session_index, "Terminating Aleph BFT session");

        // The engine has no internal stopping condition; abort it now that
        // we hold the signed outcome — peers that still need it will fetch
        // via SessionIndex/SignedSessionOutcome.
        aleph_handle.abort();
        aleph_handle.await.ok();

        self.complete_session(session_index, signed_session_outcome)
            .await;

        Some(())
    }

    pub async fn complete_signed_session_outcome(
        &self,
        session_index: u64,
        ordered_receiver: Receiver<(AlephRound, PeerId, ConsensusItem)>,
        signed_outcomes_receiver: Receiver<(PeerId, SignedSessionOutcome)>,
        signatures_receiver: Receiver<(PeerId, schnorr::Signature)>,
        connections: ReconnectP2PConnections<P2PMessage>,
    ) -> Option<SignedSessionOutcome> {
        // It is guaranteed that aleph bft will always replay all previously processed
        // items from the current session from index zero
        let mut item_index = 0;

        // We request the signed session outcome from a random peer at a fixed
        // interval (3s prod / 300ms regtest).
        let broadcast_interval = if self.cfg.consensus.network == bitcoin::Network::Regtest {
            Duration::from_millis(300)
        } else {
            Duration::from_secs(3)
        };
        let mut index_broadcast_interval = tokio::time::interval(broadcast_interval);

        // We build a session outcome out of the ordered batches until either we have
        // processed aleph_rounds_per_session rounds or a threshold signed
        // session outcome is obtained from our peers
        loop {
            tokio::select! {
                result = ordered_receiver.recv() => {
                    let (round, creator, item) = result.ok()?;

                    if round >= self.cfg.consensus.aleph_rounds_per_session {
                        info!(
                            target: LOG_CONSENSUS,
                            session_index,
                            "Reached Aleph BFT round limit, stopping item collection"
                        );
                        break;
                    }

                    if let Ok(()) = self
                        .process_consensus_item(session_index, item_index, item, creator)
                        .await
                    {
                        item_index += 1;
                    }
                },
                result = signed_outcomes_receiver.recv() => {
                    let (peer_id, p2p_outcome) = result.ok()?;

                    // Validate signatures
                    if self.validate_signed_session_outcome(&p2p_outcome, session_index) {
                        info!(
                            target: LOG_CONSENSUS,
                            session_index,
                            peer_id = %peer_id,
                            "Received SignedSessionOutcome via P2P while collection signatures"
                        );

                        let pending_accepted_items = self.pending_accepted_items().await;

                        // this panics if we have more accepted items than the signed session outcome
                        let (processed, unprocessed) = p2p_outcome
                            .session_outcome
                            .items
                            .split_at(pending_accepted_items.len());

                        info!(
                            target: LOG_CONSENSUS,
                            ?session_index,
                            processed = %processed.len(),
                            unprocessed = %unprocessed.len(),
                            "Processing remaining items..."
                        );

                        assert!(
                            processed.iter().eq(pending_accepted_items.iter()),
                            "Consensus Failure: pending accepted items disagree with federation consensus"
                        );

                        for (accepted_item, item_index) in unprocessed.iter().zip(processed.len()..) {
                            if let Err(err) = self.process_consensus_item(
                                session_index,
                                item_index as u64,
                                accepted_item.item.clone(),
                                accepted_item.peer
                            ).await {
                                panic!(
                                    "Consensus Failure: rejected item accepted by federation consensus: {accepted_item:?}, items: {}+{}, session_idx: {session_index}, item_idx: {item_index}, err: {err}",
                                    processed.len(),
                                    unprocessed.len(),
                                );
                            }
                        }

                        info!(
                            target: LOG_CONSENSUS,
                            ?session_index,
                            peer_id = %peer_id,
                            "Successfully recovered session via P2P"
                        );

                        return Some(p2p_outcome);
                    }

                    debug!(
                        target: LOG_CONSENSUS,
                        %peer_id,
                        "Invalid P2P SignedSessionOutcome"
                    );
                }
                _ = index_broadcast_interval.tick() => {
                    connections.send(
                        Recipient::Peer(self.random_peer()),
                        P2PMessage::SessionIndex(session_index),
                    );
                }
            }
        }

        let items = self.pending_accepted_items().await;

        assert_eq!(item_index, items.len() as u64);

        info!(target: LOG_CONSENSUS, ?session_index, ?item_index, "Processed all items for session");

        let session_outcome = SessionOutcome { items };

        let header = session_outcome.header(session_index);

        info!(
            target: LOG_CONSENSUS,
            ?session_index,
            "Signing session header..."
        );

        let keychain = build_keychain(&self.cfg);

        let header_hash = header.consensus_hash::<sha256::Hash>();

        let our_signature = keychain.sign(&header_hash);

        let mut signatures = BTreeMap::from_iter([(self.identity(), our_signature)]);

        // We request the session signature every second to all peers
        let mut signature_broadcast_interval = tokio::time::interval(Duration::from_secs(1));

        // We collect the ordered signatures until we either obtain a threshold
        // signature or a signed session outcome arrives from our peers
        while signatures.len() < self.num_peers().threshold() {
            tokio::select! {
                result = signatures_receiver.recv() => {
                    let (peer_id, signature) = result.ok()?;

                    if keychain.verify(&header_hash, &signature, peer_id) {
                        signatures.insert(peer_id, signature);

                        info!(
                            target: LOG_CONSENSUS,
                            session_index,
                            peer_id = %peer_id,
                            "Collected signature from peer via P2P"
                        );
                    }

                    debug!(
                        target: LOG_CONSENSUS,
                        session_index,
                        peer_id = %peer_id,
                        "Invalid P2P signature from peer"
                    );
                }
                result = signed_outcomes_receiver.recv() => {
                    let (peer_id, p2p_outcome) = result.ok()?;

                    if self.validate_signed_session_outcome(&p2p_outcome, session_index) {
                        assert_eq!(
                            header,
                            p2p_outcome.session_outcome.header(session_index),
                            "Consensus Failure: header disagrees with federation consensus"
                        );

                        info!(
                            target: LOG_CONSENSUS,
                            session_index,
                            %peer_id,
                            "Recovered session via P2P while collecting signatures"
                        );

                        return Some(p2p_outcome);
                    }

                    debug!(
                        target: LOG_CONSENSUS,
                        %peer_id,
                        "Invalid P2P SignedSessionOutcome"
                    );
                }
                _ = signature_broadcast_interval.tick() => {
                    connections.send(
                        Recipient::Everyone,
                        P2PMessage::SessionSignature(our_signature),
                    );
                }
                _ = index_broadcast_interval.tick() => {
                    connections.send(
                        Recipient::Peer(self.random_peer()),
                        P2PMessage::SessionIndex(session_index),
                    );
                }
            }
        }

        info!(
            target: LOG_CONSENSUS,
            session_index,
            "Successfully collected threshold of signatures"
        );

        Some(SignedSessionOutcome {
            session_outcome,
            signatures,
        })
    }

    /// Returns a random peer ID excluding ourselves
    fn random_peer(&self) -> PeerId {
        self.num_peers()
            .peer_ids()
            .filter(|p| *p != self.identity())
            .choose(&mut rand::thread_rng())
            .expect("We have at least three peers")
    }

    /// Validate a SignedSessionOutcome received via P2P
    fn validate_signed_session_outcome(
        &self,
        outcome: &SignedSessionOutcome,
        session_index: u64,
    ) -> bool {
        if outcome.signatures.len() != self.num_peers().threshold() {
            return false;
        }

        let keychain = build_keychain(&self.cfg);
        let header_hash = outcome
            .session_outcome
            .header(session_index)
            .consensus_hash::<sha256::Hash>();

        outcome
            .signatures
            .iter()
            .all(|(signer_id, sig)| keychain.verify(&header_hash, sig, *signer_id))
    }

    pub async fn pending_accepted_items(&self) -> Vec<AcceptedItem> {
        self.db
            .begin_read()
            .iter(&ACCEPTED_ITEM, |r| r.map(|(_, item)| item).collect())
    }

    pub async fn complete_session(
        &self,
        session_index: u64,
        signed_session_outcome: SignedSessionOutcome,
    ) {
        let tx = self.db.begin_write();

        tx.as_ref().delete_table(&ACCEPTED_ITEM);
        tx.as_ref().delete_table(&ALEPH_UNITS);

        assert!(
            tx.insert(
                &SIGNED_SESSION_OUTCOME,
                &session_index,
                &signed_session_outcome,
            )
            .is_none(),
            "We tried to overwrite a signed session outcome"
        );

        tx.commit();
    }

    #[instrument(target = LOG_CONSENSUS, skip(self, item), level = "info")]
    pub async fn process_consensus_item(
        &self,
        session_index: u64,
        item_index: u64,
        item: ConsensusItem,
        peer: PeerId,
    ) -> anyhow::Result<()> {
        trace!(
            target: LOG_CONSENSUS,
            %peer,
            item = ?item,
            "Processing consensus item"
        );

        self.ci_status_senders
            .get(&peer)
            .expect("No ci status sender for peer")
            .send_replace(Some(session_index));

        let tx = self.db.begin_write();

        // When we recover from a mid-session crash aleph bft will replay the units that
        // were already processed before the crash. We therefore skip all consensus
        // items until we have seen every previously accepted items again.
        if let Some(existing_item) = tx.get(&ACCEPTED_ITEM, &item_index) {
            if existing_item.item == item && existing_item.peer == peer {
                return Ok(());
            }

            bail!(
                "Item was discarded previously: existing: {existing_item:?} {}, current: {item:?}, {peer}",
                existing_item.peer
            );
        }

        self.process_consensus_item_with_db_transaction(&tx, item.clone(), peer)
            .await
            .inspect_err(|err| {
                // Rejected items are very common, so only trace level
                trace!(
                    target: LOG_CONSENSUS,
                    %peer,
                    item = ?item,
                    err = %format_args!("{err:#}"),
                    "Rejected consensus item"
                );
            })?;

        tx.insert(
            &ACCEPTED_ITEM,
            &item_index,
            &AcceptedItem {
                item: item.clone(),
                peer,
            },
        );

        debug!(
            target: LOG_CONSENSUS,
            %peer,
            item = ?item,
            "Processed consensus item"
        );

        tx.commit();

        Ok(())
    }

    async fn process_consensus_item_with_db_transaction(
        &self,
        tx: &WriteTransaction,
        consensus_item: ConsensusItem,
        peer_id: PeerId,
    ) -> anyhow::Result<()> {
        match consensus_item {
            ConsensusItem::Module(module_item) => {
                self.server
                    .process_consensus_item(&tx.as_ref(), &module_item, peer_id)
                    .await
            }
            ConsensusItem::Transaction(transaction) => {
                let txid = transaction.tx_hash();
                if tx.get(&ACCEPTED_TRANSACTION, &txid).is_some() {
                    debug!(
                        target: LOG_CONSENSUS,
                        %txid,
                        "Transaction already accepted"
                    );
                    bail!("Transaction is already accepted");
                }

                process_transaction_with_server(&self.server, tx, &transaction)
                    .await
                    .map_err(|error| anyhow!(error.to_string()))?;

                debug!(target: LOG_CONSENSUS, %txid,  "Transaction accepted");
                tx.insert(&ACCEPTED_TRANSACTION, &txid, &());

                let audit = self.server.audit(tx).await;

                assert!(
                    audit.total >= 0,
                    "Balance sheet of the fed has gone negative, this should never happen! {audit:?}"
                );

                Ok(())
            }
        }
    }

    /// Returns the number of sessions already saved in the database. This count
    /// **does not** include the currently running session.
    async fn get_finished_session_count(&self) -> u64 {
        get_finished_session_count_static(&self.db.begin_read()).await
    }
}

pub async fn get_finished_session_count_static(tx: &ReadTransaction) -> u64 {
    tx.iter(&SIGNED_SESSION_OUTCOME, |r| {
        r.next_back().map_or(0, |(k, _)| k + 1)
    })
}

fn build_keychain(cfg: &ServerConfig) -> AlephKeychain {
    let keypair = cfg.private.broadcast_secret_key.keypair(SECP256K1);

    let pubkeys = cfg
        .consensus
        .peers
        .iter()
        .map(|(id, ep)| (*id, ep.broadcast_pk))
        .collect();

    AlephKeychain::new(keypair, pubkeys)
}
