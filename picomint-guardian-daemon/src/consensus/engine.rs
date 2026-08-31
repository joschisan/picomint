use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::time::Duration;

use anyhow::{anyhow, ensure};
use async_channel::Receiver;
use async_trait::async_trait;
use picomint_bft::{
    Engine as BftEngine, INetwork, ItemConsumer, Keychain as BftKeychain, Round as BftRound,
};
use picomint_core::secp256k1::{SECP256K1, schnorr};
use picomint_core::session::{AcceptedItem, SessionOutcome, SignedSessionOutcome};
use picomint_core::tx::{ConsensusItem, TxError};
use picomint_core::version::{CONSENSUS_VERSION, ConsensusVersion};
use picomint_core::{NumPeers, NumPeersExt, PeerId, TransactionId};
use picomint_encoding::Encodable;
use picomint_sqlite::{Database, DbRead, ReadTx, WriteTx};
use rand::seq::IteratorRandom;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{info, instrument};

use crate::config::ServerConfig;
use crate::consensus::bft::{DataProvider, Network};
use crate::consensus::db::{
    AcceptedItemTable, AcceptedTxTable, BftEmittedTable, BftUnitsTable, ConsensusVersionVoteTable,
    SessionCutTable, SignedSessionOutcomeTable, consensus_version,
};
use crate::consensus::server::{Server, process_tx_with_server};
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
/// walks. A count is what bounds that, and it is a cut on the same terms as
/// the byte budget: checked after every accepted item, in the same tx that
/// accepted it, so the cut point commits atomically with the count crossing.
///
/// Under load the byte budget cuts first, so what this sets is the cadence of
/// an idle federation — the block count votes two modules cast per peer per
/// block are all such a session collects, which is a session every few days.
const SESSION_OUTCOME_ITEM_LIMIT: u64 = 10_000;

/// Runs the main server consensus loop
pub struct ConsensusEngine {
    pub server: Server,
    pub db: Database,
    pub cfg: ServerConfig,
    pub submission_rx: Receiver<ConsensusItem>,
    pub connections: ReconnectP2PConnections<P2PMessage>,
    pub tx_reject_tx: broadcast::Sender<(TransactionId, TxError)>,
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
        }
    }

    pub async fn run_session(
        &self,
        connections: ReconnectP2PConnections<P2PMessage>,
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
        let (outcomes_tx, outcomes_rx) = async_channel::bounded(self.num_peers().total());
        let (signatures_tx, signatures_rx) = async_channel::bounded(self.num_peers().total());

        let network = Network::new(
            connections.clone(),
            outcomes_tx,
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
            self.item_processor(),
            BftUnitsTable,
            BftEmittedTable,
        );

        let bft_handle = tokio::spawn(bft_engine.run());

        let signed_session_outcome = self
            .complete_signed_session_outcome(
                session_index,
                outcomes_rx,
                signatures_rx,
                connections,
                bft_handle,
            )
            .await?;

        assert!(
            self.validate_signed_session_outcome(&signed_session_outcome, session_index),
            "Our created signed session outcome fails validation"
        );

        self.complete_session(session_index, signed_session_outcome)
            .await;

        Some(())
    }

    /// A fresh [`ItemProcessor`] for the running session, its budget
    /// counters and cut flag seeded from what the session has already
    /// accepted — after a restart the count resumes where the prior run
    /// durably left it, so the session cuts where its peers do.
    fn item_processor(&self) -> ItemProcessor {
        let dbtx = self.db.begin_read();

        let n_bytes = dbtx.iter(&AcceptedItemTable, |r| {
            r.map(|entry| entry.1.item.consensus_encode_to_vec().len())
                .sum()
        });

        let n_items = dbtx.iter(&AcceptedItemTable, |r| r.count() as u64);

        let cut = dbtx.get(&SessionCutTable, &()).is_some();

        ItemProcessor {
            server: self.server.clone(),
            identity: self.identity(),
            num_peers: self.num_peers(),
            default_version: self.cfg.consensus.default_version,
            rounds_per_session: rounds_per_session(&self.cfg),
            tx_reject_tx: self.tx_reject_tx.clone(),
            n_bytes,
            n_items,
            cut,
        }
    }

    pub async fn complete_signed_session_outcome(
        &self,
        session_index: u64,
        outcomes_rx: Receiver<(PeerId, SignedSessionOutcome)>,
        signatures_rx: Receiver<(PeerId, schnorr::Signature)>,
        connections: ReconnectP2PConnections<P2PMessage>,
        bft_handle: JoinHandle<()>,
    ) -> Option<SignedSessionOutcome> {
        // We request the signed session outcome from a random peer at a fixed
        // interval (3s prod / 300ms regtest).
        let broadcast_interval = if self.cfg.consensus.network == bitcoin::Network::Regtest {
            Duration::from_millis(300)
        } else {
            Duration::from_secs(3)
        };
        let mut index_broadcast_interval = tokio::time::interval(broadcast_interval);

        let cut_notify = self.db.notify_for_table(&SessionCutTable);

        // Items are processed inline by the bft engine's [`ItemProcessor`],
        // so all that remains here is to wait until either the processor
        // cuts the session or a peer hands us the signed outcome outright.
        let recovered = loop {
            let notified = cut_notify.notified();

            if self.db.begin_read().get(&SessionCutTable, &()).is_some() {
                break None;
            }

            tokio::select! {
                _ = notified => {}
                result = outcomes_rx.recv() => {
                    let (peer, p2p_outcome) = result.ok()?;

                    if self.validate_signed_session_outcome(&p2p_outcome, session_index) {
                        break Some((peer, p2p_outcome));
                    }
                }
                _ = index_broadcast_interval.tick() => {
                    connections.send(
                        Recipient::Peer(self.random_peer()),
                        P2PMessage::SessionIndex(session_index),
                    );
                }
            }
        };

        if let Some((peer, p2p_outcome)) = recovered {
            info!(
                session_index,
                %peer,
                "Received SignedSessionOutcome via P2P while collecting items"
            );

            // The bft engine writes accepted items inline; stop it — and
            // wait until it actually has — before reading and extending
            // them, or the tail below would race its deliveries.
            bft_handle.abort();
            bft_handle.await.ok();

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

            let processor = self.item_processor();

            let dbtx = self.db.begin_write();

            for (accepted_item, index) in unprocessed.iter().zip(processed.len() as u64..) {
                processor
                    .process_item(&dbtx, accepted_item.peer, &accepted_item.item)
                    .await
                    .expect("Rejected item accepted by federation consensus");

                dbtx.insert(&AcceptedItemTable, &index, accepted_item);
            }

            dbtx.commit();

            info!(
                ?session_index,
                %peer,
                "Successfully recovered session via P2P"
            );

            return Some(p2p_outcome);
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
        // signature or a signed session outcome arrives from our peers. The
        // bft engine keeps running throughout: its delivery is done, but its
        // anti-entropy is what lets lagging peers reach their own cut and
        // contribute the signatures we are waiting on.
        let signed_session_outcome = loop {
            if signatures.len() >= self.num_peers().threshold() {
                info!(
                    session_index,
                    "Successfully collected threshold of signatures"
                );

                break SignedSessionOutcome {
                    session_outcome,
                    signatures,
                };
            }

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

                        break p2p_outcome;
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
        };

        info!(?session_index, "Terminating BFT session");

        // The engine has no internal stopping condition; abort it now that
        // we hold the signed outcome — peers that still need it will fetch
        // via SessionIndex/SignedSessionOutcome.
        bft_handle.abort();
        bft_handle.await.ok();

        Some(signed_session_outcome)
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
        let dbtx = self.db.begin_write();

        dbtx.clear_table(&AcceptedItemTable);

        dbtx.clear_table(&BftUnitsTable);

        dbtx.clear_table(&BftEmittedTable);

        dbtx.clear_table(&SessionCutTable);

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

    /// Returns the number of sessions already saved in the database. This count
    /// **does not** include the currently running session.
    async fn get_finished_session_count(&self) -> u64 {
        get_finished_session_count_static(&self.db.begin_read()).await
    }
}

/// The bft engine's [`ItemConsumer`]: applies each ordered item inside the
/// tx that delivered it, scoped in a savepoint so a rejected item's writes
/// roll back while the delivery itself commits. Accepted items land in
/// [`AcceptedItemTable`] under a dense index, and once a session limit is
/// crossed the processor writes [`SessionCutTable`] — in that same tx, so
/// the cut point is durable exactly alongside the items it finalizes — and
/// breaks delivery for the session's remainder.
pub struct ItemProcessor {
    server: Server,
    identity: PeerId,
    num_peers: NumPeers,
    default_version: ConsensusVersion,
    rounds_per_session: BftRound,
    tx_reject_tx: broadcast::Sender<(TransactionId, TxError)>,
    n_bytes: usize,
    n_items: u64,
    cut: bool,
}

#[async_trait]
impl ItemConsumer<ConsensusItem> for ItemProcessor {
    async fn process(
        &mut self,
        dbtx: &WriteTx,
        round: BftRound,
        creator: PeerId,
        item: ConsensusItem,
    ) -> ControlFlow<()> {
        if self.cut {
            return ControlFlow::Break(());
        }

        // The round cap cuts *before* the item: its item is not part of
        // the session, on any peer.
        if round >= self.rounds_per_session {
            return self.cut_session(dbtx);
        }

        let savepoint = dbtx.savepoint();

        match self.process_item(dbtx, creator, &item).await {
            Ok(()) => {
                self.n_bytes += item.consensus_encode_to_vec().len();

                let accepted = AcceptedItem {
                    peer: creator,
                    item,
                };

                dbtx.insert(&AcceptedItemTable, &self.n_items, &accepted);

                savepoint.release();

                self.n_items += 1;
            }
            Err(..) => drop(savepoint),
        }

        // The budget caps cut *after* the item that crossed them: the item
        // is part of the session, and checking here rather than ahead of
        // the next delivery makes the cut commit atomically with the
        // counters crossing — a crash cannot fall between them.
        if self.n_bytes >= SESSION_OUTCOME_BYTE_TARGET || self.n_items >= SESSION_OUTCOME_ITEM_LIMIT
        {
            return self.cut_session(dbtx);
        }

        ControlFlow::Continue(())
    }
}

impl ItemProcessor {
    fn cut_session(&mut self, dbtx: &WriteTx) -> ControlFlow<()> {
        dbtx.insert(&SessionCutTable, &(), &());

        self.cut = true;

        ControlFlow::Break(())
    }

    #[instrument(skip(self, dbtx, item), level = "info")]
    async fn process_item(
        &self,
        dbtx: &WriteTx,
        peer: PeerId,
        item: &ConsensusItem,
    ) -> anyhow::Result<()> {
        match item {
            ConsensusItem::Module(ci) => {
                self.server.process_module_ci(dbtx, peer, ci).await?;
            }
            ConsensusItem::Tx(tx) => {
                let txid = tx.compute_txid();

                ensure!(
                    dbtx.get(&AcceptedTxTable, &txid).is_none(),
                    "Transaction is already accepted"
                );

                if let Err(error) = process_tx_with_server(&self.server, dbtx, tx).await {
                    // Only our own submission has a submission RPC waiting on
                    // it, and copies of an already accepted transaction bail at
                    // the check above - so every rejection we broadcast is
                    // final and has a caller to fail.
                    if peer == self.identity {
                        self.tx_reject_tx.send((txid, error.clone())).ok();
                    }

                    return Err(anyhow!(error.to_string()));
                }

                dbtx.insert(&AcceptedTxTable, &txid, &());

                let audit = self.server.audit(dbtx).await;

                assert!(audit.total >= 0, "Failed audit: {audit:?}");
            }
            ConsensusItem::Version(vote) => {
                let current_vote = dbtx
                    .insert(&ConsensusVersionVoteTable, &peer, vote)
                    .unwrap_or(self.default_version);

                ensure!(current_vote < *vote, "Consensus version vote is redundant");

                // A threshold has moved past what we know how to apply, so
                // every rule we would run from here on is the wrong one.
                // Halting is the only correct move left.
                assert!(
                    consensus_version(dbtx, self.num_peers, self.default_version)
                        <= CONSENSUS_VERSION,
                    "Guardian does not support the active consensus version, please upgrade"
                );
            }
        }

        Ok(())
    }
}

pub async fn get_finished_session_count_static(dbtx: &ReadTx) -> u64 {
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
