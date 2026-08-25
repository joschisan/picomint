//! Adapters binding `picomint-bft` to the daemon's transport and
//! mempool — an `INetwork` impl over `ReconnectP2PConnections<P2PMessage>`
//! and a `DataProvider` impl pulling from the submission channel.
//!
//! Storage is owned by `picomint-bft` directly via the redb table
//! `BFT_UNITS`. The application consumer that drains
//! committed items lives in [`crate::consensus::engine`] and receives
//! through the `ordered_tx` channel passed to `BftEngine::new`.

use std::collections::VecDeque;
use std::future::pending;

use async_channel::{Receiver, Sender};
use async_trait::async_trait;
use picomint_bft::{
    DataProvider as BftDataProvider, INetwork, Message as BftMessage, Recipient as BftRecipient,
};
use picomint_core::PeerId;
use picomint_core::config::BFT_UNIT_BYTE_TARGET;
use picomint_core::secp256k1::schnorr;
use picomint_core::session::SignedSessionOutcome;
use picomint_core::tx::ConsensusItem;
use picomint_encoding::Encodable;
use picomint_redb::Database;
use tracing::error;

use crate::consensus::db::SignedSessionOutcomeTable;
use crate::p2p::{P2PMessage, Recipient as P2PRecipient, ReconnectP2PConnections};

/// `INetwork` adapter wrapping `ReconnectP2PConnections<P2PMessage>`.
/// Bft traffic flows on the `P2PMessage::Bft` variant; non-bft
/// variants (`SessionSignature`, `SessionIndex`, `SignedSessionOutcome`)
/// are dispatched to their respective channels here so the engine sees
/// only `bft::Message` on `receive`.
///
/// Session isolation is handled inside the engine — every unit carries
/// its own `session` field, and the graph rejects mismatches — so the
/// adapter forwards bft traffic uninterpreted regardless of session.
pub struct Network {
    connections: ReconnectP2PConnections<P2PMessage>,
    signed_outcomes_tx: Sender<(PeerId, SignedSessionOutcome)>,
    signatures_tx: Sender<(PeerId, schnorr::Signature)>,
    db: Database,
}

impl Network {
    pub fn new(
        connections: ReconnectP2PConnections<P2PMessage>,
        signed_outcomes_tx: Sender<(PeerId, SignedSessionOutcome)>,
        signatures_tx: Sender<(PeerId, schnorr::Signature)>,
        db: Database,
    ) -> Self {
        Self {
            connections,
            signed_outcomes_tx,
            signatures_tx,
            db,
        }
    }
}

fn into_p2p_recipient(r: BftRecipient) -> P2PRecipient {
    match r {
        BftRecipient::Everyone => P2PRecipient::Everyone,
        BftRecipient::Peer(p) => P2PRecipient::Peer(p),
    }
}

#[async_trait]
impl INetwork<ConsensusItem> for Network {
    fn send(&self, recipient: BftRecipient, msg: BftMessage<ConsensusItem>) {
        self.connections
            .send(into_p2p_recipient(recipient), P2PMessage::Bft(msg));
    }

    async fn receive(&self) -> Option<(PeerId, BftMessage<ConsensusItem>)> {
        loop {
            let (peer, message) = self.connections.receive().await?;

            match message {
                P2PMessage::Bft(msg) => {
                    return Some((peer, msg));
                }
                P2PMessage::SessionSignature(signature) => {
                    self.signatures_tx.try_send((peer, signature)).ok();
                }
                P2PMessage::SessionIndex(their_session) => {
                    if let Some(outcome) = self
                        .db
                        .begin_read()
                        .get(&SignedSessionOutcomeTable, &their_session)
                    {
                        self.connections.send(
                            P2PRecipient::Peer(peer),
                            P2PMessage::SignedSessionOutcome(outcome),
                        );
                    }
                }
                P2PMessage::SignedSessionOutcome(outcome) => {
                    self.signed_outcomes_tx.try_send((peer, outcome)).ok();
                }
                message => error!(
                    %peer,
                    ?message,
                    "Received unexpected p2p message variant"
                ),
            }
        }
    }

    async fn receive_from_peer(&self, _peer: PeerId) -> Option<BftMessage<ConsensusItem>> {
        unimplemented!("bft consensus only uses fan-in receive")
    }
}

/// `DataProvider` impl draining the daemon's submission channel into the
/// next unit's payload, cut off once the payload reaches
/// [`BFT_UNIT_BYTE_TARGET`]. `pending_items` holds items pulled off the
/// channel by `wait_for_data` to wake the engine out of quiescence.
pub struct DataProvider {
    submission_rx: Receiver<ConsensusItem>,
    pending_items: VecDeque<ConsensusItem>,
}

impl DataProvider {
    pub fn new(submission_rx: Receiver<ConsensusItem>) -> Self {
        Self {
            submission_rx,
            pending_items: VecDeque::new(),
        }
    }

    fn next_item(&mut self) -> Option<ConsensusItem> {
        self.pending_items
            .pop_front()
            .or_else(|| self.submission_rx.try_recv().ok())
    }
}

#[async_trait]
impl BftDataProvider<ConsensusItem> for DataProvider {
    fn get_data(&mut self) -> Vec<ConsensusItem> {
        let mut items = Vec::new();

        // The target bounds a unit's memory footprint, nothing consensus
        // depends on, so no item is ever dropped to honour it: we take
        // whatever gets us there and stop.
        while let Some(item) = self.next_item() {
            items.push(item);

            if items.consensus_encode_to_vec().len() >= BFT_UNIT_BYTE_TARGET {
                break;
            }
        }

        items
    }

    async fn wait_for_data(&mut self) {
        match self.submission_rx.recv().await {
            Ok(item) => self.pending_items.push_back(item),
            // A closed channel means the daemon is shutting down; park
            // instead of busy-waking the engine.
            Err(_) => pending().await,
        }
    }
}
