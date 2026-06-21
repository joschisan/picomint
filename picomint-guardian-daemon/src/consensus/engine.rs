use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{anyhow, ensure};
use async_channel::Receiver;
use futures::StreamExt;
use picomint_bft::{Engine as BftEngine, INetwork, Keychain as BftKeychain, Round as BftRound};
use picomint_core::secp256k1::{SECP256K1, schnorr};
use picomint_core::session::{AcceptedItem, SessionOutcome, SignedSessionOutcome};
use picomint_core::tx::ConsensusItem;
use picomint_core::{NumPeers, NumPeersExt, PeerId};
use picomint_redb::{Database, ReadTx, WriteTx};
use rand::seq::IteratorRandom;
use tokio::sync::watch;
use tokio::time::sleep;
use tracing::{info, instrument};

use crate::config::ServerConfig;
use crate::consensus::bft::{DataProvider, Network};
use crate::consensus::db::{
    AcceptedItemTable, AcceptedTxTable, BftCosigsTable, BftUnitsTable, SignedSessionOutcomeTable,
    drop_bft_tables,
};
use crate::consensus::server::process_tx_with_server;
use crate::p2p::{P2PMessage, Recipient, ReconnectP2PConnections};

/// Runs the main server consensus loop
pub struct ConsensusEngine {
    pub server: crate::consensus::server::Server,
    pub db: Database,
    pub cfg: ServerConfig,
    pub submission_rx: Receiver<ConsensusItem>,
    pub shutdown_rx: watch::Receiver<Option<u64>>,
    pub connections: ReconnectP2PConnections<P2PMessage>,
}

impl ConsensusEngine {
    fn num_peers(&self) -> NumPeers {
        self.cfg.consensus.peers.to_num_peers()
    }

    fn identity(&self) -> PeerId {
        self.cfg.private.identity
    }

    #[instrument(name = "run", skip_all, fields(id=%self.cfg.private.identity))]
    pub async fn run(self) -> anyhow::Result<()> {
        // We need four peers to run the atomic broadcast
        assert!(self.num_peers().total() >= 4);

        loop {
            let session_index = self.get_finished_session_count().await;

            info!(session_index, "Starting consensus session");

            if self
                .run_session(self.connections.clone(), session_index)
                .await
                .is_none()
            {
                return Ok(());
            }

            info!(?session_index, "Completed consensus session");

            if Some(session_index) == self.shutdown_rx.borrow().to_owned() {
                info!("Initiating shutdown, waiting for peers to complete the session...");

                sleep(Duration::from_mins(1)).await;

                break;
            }
        }

        info!("Consensus task shut down");

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
        // the end of the bft session would only be reached after a minimum
        // of 10 years. In case of such an attack the broadcast stops ordering any
        // items until the attack subsides as no items are ordered while the
        // signatures are collected. The maximum RAM consumption of the bft
        // broadcast instance is therefore bound by:
        //
        // self.keychain.peer_count()
        //      * (broadcast_rounds_per_session + 1000)
        //      * BFT_UNIT_BYTE_LIMIT

        /// BFT round delay (ms). Byzantine-fault-only; the ordering floor is
        /// dominated by network latency in practice.
        const ROUND_DELAY_MS: f64 = 50.0;
        const BASE: f64 = 1.02;

        let rounds_per_session = self.cfg.consensus.bft_rounds_per_session;

        let unit_delay = Box::new(move |round_index: u16| {
            let delay = if round_index == 0 {
                0.0
            } else {
                ROUND_DELAY_MS * BASE.powf(round_index.saturating_sub(rounds_per_session) as f64)
            };

            Duration::from_millis(delay.round() as u64)
        });

        let (signed_outcomes_tx, signed_outcomes_rx) = async_channel::unbounded();
        let (signatures_tx, signatures_rx) = async_channel::unbounded();
        let (ordered_tx, ordered_rx) = async_channel::unbounded();

        let network = Network::new(
            connections.clone(),
            signed_outcomes_tx,
            signatures_tx,
            self.db.clone(),
        )
        .into_dyn();

        let bft_engine = BftEngine::new(
            self.identity(),
            session_index,
            self.num_peers(),
            self.db.clone(),
            build_keychain(&self.cfg),
            network,
            DataProvider::new(self.submission_rx.clone()),
            unit_delay,
            ordered_tx,
            BftUnitsTable,
            BftCosigsTable,
        );

        let bft_handle = tokio::spawn(bft_engine.run());

        let signed_session_outcome = self
            .complete_signed_session_outcome(
                session_index,
                signed_outcomes_rx,
                signatures_rx,
                ordered_rx,
                connections,
            )
            .await?;

        assert!(
            self.validate_signed_session_outcome(&signed_session_outcome, session_index),
            "Our created signed session outcome fails validation"
        );

        info!(?session_index, "Terminating BFT session");

        // The engine has no internal stopping condition; abort it now that
        // we hold the signed outcome — peers that still need it will fetch
        // via SessionIndex/SignedSessionOutcome.
        bft_handle.abort();
        bft_handle.await.ok();

        self.complete_session(session_index, signed_session_outcome)
            .await;

        Some(())
    }

    pub async fn complete_signed_session_outcome(
        &self,
        session_index: u64,
        signed_outcomes_rx: Receiver<(PeerId, SignedSessionOutcome)>,
        signatures_rx: Receiver<(PeerId, schnorr::Signature)>,
        ordered_rx: Receiver<(BftRound, PeerId, ConsensusItem)>,
        connections: ReconnectP2PConnections<P2PMessage>,
    ) -> Option<SignedSessionOutcome> {
        // We request the signed session outcome from a random peer at a fixed
        // interval (3s prod / 300ms regtest).
        let broadcast_interval = if self.cfg.consensus.network == bitcoin::Network::Regtest {
            Duration::from_millis(300)
        } else {
            Duration::from_secs(3)
        };
        let mut index_broadcast_interval = tokio::time::interval(broadcast_interval);

        // We enumerate every bft delivery for this session; ACCEPTED_ITEM
        // is sparse (rejected positions are absent). On crash replay bft
        // re-emits from position 0, so we skip past the highest index
        // already in AcceptedItemTable — every position up to and
        // including it was already processed (accepted *or* rejected) by
        // the prior run.
        let skip = self
            .db
            .begin_read()
            .iter(&AcceptedItemTable, |r| r.next_back().map(|(k, _)| k))
            .map_or(0, |k| k as usize + 1);

        let mut ordered_rx = Box::pin(ordered_rx.enumerate().skip(skip));

        // We build a session outcome out of the ordered batches until either we have
        // processed bft_rounds_per_session rounds or a threshold signed
        // session outcome is obtained from our peers
        loop {
            tokio::select! {
                result = ordered_rx.next() => {
                    let (index, (round, creator, item)) = result?;

                    if round >= self.cfg.consensus.bft_rounds_per_session {
                        break;
                    }

                    let dbtx = self.db.begin_write_relaxed();

                    if self.process_consensus_item(&dbtx, index as u64, creator, item).await.is_ok() {
                        dbtx.commit();
                    }
                },
                result = signed_outcomes_rx.recv() => {
                    let (peer, p2p_outcome) = result.ok()?;

                    // Validate signatures
                    if self.validate_signed_session_outcome(&p2p_outcome, session_index) {
                        info!(
                            session_index,
                            peer = %peer,
                            "Received SignedSessionOutcome via P2P while collection signatures"
                        );

                        let pending_accepted_items = self.pending_accepted_items().await;

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

                        let dbtx = self.db.begin_write_relaxed();

                        for accepted_item in unprocessed {
                            self.process_consensus_item(
                                &dbtx,
                                accepted_item.index,
                                accepted_item.peer,
                                accepted_item.item.clone(),
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
                        Recipient::Peer(self.random_peer()),
                        P2PMessage::SessionIndex(session_index),
                    );
                }
            }
        }

        let items = self.pending_accepted_items().await;

        let session_outcome = SessionOutcome { items };

        let header = session_outcome.header(session_index);

        info!(?session_index, "Signing session header...");

        let keychain = build_keychain(&self.cfg);

        let our_signature = keychain.sign(session_index, &header);

        let mut signatures = BTreeMap::from_iter([(self.identity(), our_signature)]);

        // We request the session signature every second to all peers
        let mut signature_broadcast_interval = tokio::time::interval(Duration::from_secs(1));

        // We collect the ordered signatures until we either obtain a threshold
        // signature or a signed session outcome arrives from our peers
        while signatures.len() < self.num_peers().threshold() {
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
                result = signed_outcomes_rx.recv() => {
                    let (peer, p2p_outcome) = result.ok()?;

                    if self.validate_signed_session_outcome(&p2p_outcome, session_index) {
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
                        Recipient::Peer(self.random_peer()),
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

        let header = outcome.session_outcome.header(session_index);

        let keychain = build_keychain(&self.cfg);

        outcome
            .signatures
            .iter()
            .all(|(signer_id, sig)| keychain.verify(session_index, &header, sig, *signer_id))
    }

    pub async fn pending_accepted_items(&self) -> Vec<AcceptedItem> {
        self.db
            .begin_read()
            .iter(&AcceptedItemTable, |r| r.map(|(_, item)| item).collect())
    }

    pub async fn complete_session(
        &self,
        session_index: u64,
        signed_session_outcome: SignedSessionOutcome,
    ) {
        let dbtx = self.db.begin_write_relaxed();

        dbtx.delete_table(&AcceptedItemTable);

        drop_bft_tables(&dbtx);

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

    #[instrument(skip(self, dbtx, item), level = "info")]
    pub async fn process_consensus_item(
        &self,
        dbtx: &WriteTx,
        index: u64,
        peer: PeerId,
        item: ConsensusItem,
    ) -> anyhow::Result<()> {
        match item.clone() {
            ConsensusItem::Module(ci) => {
                self.server.process_module_ci(dbtx, peer, &ci).await?;
            }
            ConsensusItem::Tx(tx) => {
                let txid = tx.compute_txid();

                ensure!(
                    dbtx.get(&AcceptedTxTable, &txid).is_none(),
                    "Transaction is already accepted"
                );

                process_tx_with_server(&self.server, dbtx, &tx)
                    .await
                    .map_err(|error| anyhow!(error.to_string()))?;

                dbtx.insert(&AcceptedTxTable, &txid, &());

                let audit = self.server.audit(dbtx).await;

                assert!(audit.total >= 0, "Failed audit: {audit:?}");
            }
        }

        dbtx.insert(
            &AcceptedItemTable,
            &index,
            &AcceptedItem { index, peer, item },
        );

        Ok(())
    }

    /// Returns the number of sessions already saved in the database. This count
    /// **does not** include the currently running session.
    async fn get_finished_session_count(&self) -> u64 {
        get_finished_session_count_static(&self.db.begin_read()).await
    }
}

pub async fn get_finished_session_count_static(dbtx: &ReadTx) -> u64 {
    dbtx.iter(&SignedSessionOutcomeTable, |r| {
        r.next_back().map_or(0, |(k, _)| k + 1)
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
