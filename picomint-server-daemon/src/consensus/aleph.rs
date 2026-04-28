//! Adapters binding `picomint-aleph-bft` to the daemon's transport,
//! mempool, and storage — a `Network` impl over
//! `ReconnectP2PConnections<P2PMessage>`, a `DataProvider` impl pulling
//! from the submission channel, and a `Backup` impl backed by redb.

use async_channel::{Receiver, Sender};
use async_trait::async_trait;
use picomint_aleph_bft::{
    Backup as AlephBackup, DataProvider as AlephDataProvider, Entry as AlephEntry, INetwork,
    Message as AlephMessage, Recipient as AlephRecipient,
};
use picomint_core::PeerId;
use picomint_core::config::ALEPH_BFT_UNIT_BYTE_LIMIT;
use picomint_core::secp256k1::schnorr;
use picomint_core::session_outcome::SignedSessionOutcome;
use picomint_core::transaction::ConsensusItem;
use picomint_encoding::Encodable;
use picomint_logging::LOG_CONSENSUS;
use picomint_redb::Database;
use tracing::{error, warn};

use crate::consensus::db::{ALEPH_UNITS, SIGNED_SESSION_OUTCOME};
use crate::p2p::{P2PMessage, Recipient as P2PRecipient, ReconnectP2PConnections};

/// `INetwork` adapter wrapping `ReconnectP2PConnections<P2PMessage>`.
/// Aleph traffic flows on the `P2PMessage::Aleph` variant; non-aleph
/// variants (`SessionSignature`, `SessionIndex`, `SignedSessionOutcome`)
/// are dispatched to their respective channels here so the engine sees
/// only `aleph_bft::Message<ConsensusItem>` on `receive`.
///
/// Session isolation is handled inside the engine — every unit carries
/// its own `session` field, and the graph rejects mismatches — so the
/// adapter forwards aleph traffic uninterpreted regardless of session.
pub struct Network {
    connections: ReconnectP2PConnections<P2PMessage>,
    signed_outcomes_sender: Sender<(PeerId, SignedSessionOutcome)>,
    signatures_sender: Sender<(PeerId, schnorr::Signature)>,
    db: Database,
}

impl Network {
    pub fn new(
        connections: ReconnectP2PConnections<P2PMessage>,
        signed_outcomes_sender: Sender<(PeerId, SignedSessionOutcome)>,
        signatures_sender: Sender<(PeerId, schnorr::Signature)>,
        db: Database,
    ) -> Self {
        Self {
            connections,
            signed_outcomes_sender,
            signatures_sender,
            db,
        }
    }
}

fn into_p2p_recipient(r: AlephRecipient) -> P2PRecipient {
    match r {
        AlephRecipient::Everyone => P2PRecipient::Everyone,
        AlephRecipient::Peer(p) => P2PRecipient::Peer(p),
    }
}

#[async_trait]
impl INetwork<AlephMessage<ConsensusItem>> for Network {
    fn send(&self, recipient: AlephRecipient, msg: AlephMessage<ConsensusItem>) {
        self.connections
            .send(into_p2p_recipient(recipient), P2PMessage::Aleph(msg));
    }

    async fn receive(&self) -> Option<(PeerId, AlephMessage<ConsensusItem>)> {
        loop {
            let (peer_id, message) = self.connections.receive().await?;

            match message {
                P2PMessage::Aleph(msg) => {
                    return Some((peer_id, msg));
                }
                P2PMessage::SessionSignature(signature) => {
                    self.signatures_sender.try_send((peer_id, signature)).ok();
                }
                P2PMessage::SessionIndex(their_session) => {
                    if let Some(outcome) = self
                        .db
                        .begin_read()
                        .get(&SIGNED_SESSION_OUTCOME, &their_session)
                    {
                        self.connections.send(
                            P2PRecipient::Peer(peer_id),
                            P2PMessage::SignedSessionOutcome(outcome),
                        );
                    }
                }
                P2PMessage::SignedSessionOutcome(outcome) => {
                    self.signed_outcomes_sender
                        .try_send((peer_id, outcome))
                        .ok();
                }
                message => error!(
                    target: LOG_CONSENSUS,
                    %peer_id,
                    ?message,
                    "Received unexpected p2p message variant"
                ),
            }
        }
    }

    async fn receive_from_peer(&self, _peer: PeerId) -> Option<AlephMessage<ConsensusItem>> {
        unimplemented!("aleph consensus only uses fan-in receive")
    }
}

/// `DataProvider` impl draining the daemon's submission channel into the
/// next unit's payload, capped at [`ALEPH_BFT_UNIT_BYTE_LIMIT`] bytes
/// of encoded payload per unit. The first item that would push the
/// payload past the cap is stashed in `leftover_item` and tried again
/// on the next call — without this the engine could create an oversize
/// unit whenever the submission channel had a backlog and would then
/// trip the `Accepted` assertion in `advance_round` against
/// `Graph::insert_unit`'s own size check.
pub struct DataProvider {
    submission_receiver: Receiver<ConsensusItem>,
    leftover_item: Option<ConsensusItem>,
}

impl DataProvider {
    pub fn new(submission_receiver: Receiver<ConsensusItem>) -> Self {
        Self {
            submission_receiver,
            leftover_item: None,
        }
    }
}

#[async_trait]
impl AlephDataProvider<ConsensusItem> for DataProvider {
    async fn get_data(&mut self) -> Vec<ConsensusItem> {
        // `Vec<T>` consensus encoding is a `u32` length prefix followed
        // by the concatenated item encodings — start the budget at 4 to
        // account for the prefix.
        let mut n_bytes: usize = 4;
        let mut items = Vec::new();

        if let Some(item) = self.leftover_item.take() {
            let item_bytes = item.consensus_encode_to_vec().len();

            if n_bytes + item_bytes <= ALEPH_BFT_UNIT_BYTE_LIMIT {
                n_bytes += item_bytes;
                items.push(item);
            } else {
                // Larger than an entire unit's worth of payload —
                // dropping is the only option; it would never fit no
                // matter how many calls we deferred it across.
                warn!(
                    target: LOG_CONSENSUS,
                    ?item,
                    "Consensus item exceeds ALEPH_BFT_UNIT_BYTE_LIMIT; dropped"
                );
            }
        }

        while let Ok(item) = self.submission_receiver.try_recv() {
            let item_bytes = item.consensus_encode_to_vec().len();

            if n_bytes + item_bytes <= ALEPH_BFT_UNIT_BYTE_LIMIT {
                n_bytes += item_bytes;
                items.push(item);
            } else {
                self.leftover_item = Some(item);
                break;
            }
        }

        items
    }
}

/// `Backup<ConsensusItem>` impl persisting each `Entry` into the
/// `ALEPH_UNITS` table keyed by `(round, creator)`. Overwrites in place
/// on every save; loading iterates the table in natural key order, which
/// is `(round, peer)` lex order — i.e. the order the engine expects for
/// restore (parents before children).
pub struct RedbBackup {
    db: Database,
}

impl RedbBackup {
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

impl AlephBackup<ConsensusItem> for RedbBackup {
    fn save(&self, entry: &AlephEntry<ConsensusItem>) {
        let key = (entry.unit().round, entry.unit().creator);

        let tx = self.db.begin_write();
        tx.insert(&ALEPH_UNITS, &key, entry);
        tx.commit();
    }

    fn load(&self) -> Vec<AlephEntry<ConsensusItem>> {
        self.db
            .begin_read()
            .iter(&ALEPH_UNITS, |rows| rows.map(|(_, entry)| entry).collect())
    }
}
