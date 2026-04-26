use async_channel::Sender;
use picomint_core::PeerId;
use picomint_core::secp256k1::schnorr;
use picomint_core::session_outcome::SignedSessionOutcome;
use picomint_core::transaction::ConsensusItem;
use picomint_encoding::{Decodable, Encodable};
use picomint_logging::LOG_CONSENSUS;
use picomint_redb::Database;
use tracing::error;

use super::super::db::SIGNED_SESSION_OUTCOME;
use super::data_provider::is_valid;
use super::keychain::Keychain;
use crate::p2p::{P2PMessage, Recipient, ReconnectP2PConnections};

pub type NetworkData = aleph_bft::NetworkData<
    Vec<ConsensusItem>,
    <Keychain as aleph_bft::Keychain>::Signature,
    <Keychain as aleph_bft::MultiKeychain>::PartialMultisignature,
>;

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

#[async_trait::async_trait]
impl aleph_bft::Network<NetworkData> for Network {
    fn send(&self, network_data: NetworkData, recipient: Recipient) {
        self.connections.send(
            recipient,
            P2PMessage::Aleph(network_data.consensus_encode_to_vec()),
        );
    }

    async fn next_event(&mut self) -> Option<NetworkData> {
        loop {
            let (peer_id, message) = self.connections.receive().await?;

            match message {
                P2PMessage::Aleph(bytes) => {
                    match NetworkData::consensus_decode_partial(&mut bytes.as_slice()) {
                        Ok(network_data) => {
                            // in order to bound the RAM consumption of a session we have to bound
                            // the size of an individual unit in memory
                            if network_data
                                .included_data()
                                .iter()
                                .all(|items| is_valid(items.as_slice()))
                            {
                                return Some(network_data);
                            }

                            error!(
                                target: LOG_CONSENSUS,
                                %peer_id,
                                "Received invalid unit data"
                            );
                        }
                        Err(err) => {
                            error!(
                                target: LOG_CONSENSUS,
                                %peer_id,
                                err = %err,
                                "Failed to decode Aleph BFT network data"
                            );
                        }
                    }
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
                            Recipient::Peer(peer_id),
                            P2PMessage::SignedSessionOutcome(outcome),
                        );
                    }
                }
                P2PMessage::SignedSessionOutcome(outcome) => {
                    self.signed_outcomes_sender
                        .try_send((peer_id, outcome))
                        .ok();
                }
                message => {
                    error!(
                        target: LOG_CONSENSUS,
                        %peer_id,
                        ?message,
                        "Received unexpected p2p message variant"
                    );
                }
            }
        }
    }
}
