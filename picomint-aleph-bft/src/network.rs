use std::collections::BTreeMap;
use std::sync::Arc;

use async_channel::{Receiver, Sender};
use async_trait::async_trait;
use picomint_core::secp256k1::schnorr;
use picomint_core::{NumPeers, PeerId};
use picomint_encoding::{Decodable, Encodable};

use crate::unit::{Round, Unit, UnitData};

/// Per-recipient probability of silently dropping a message in the mock
/// network. Each unicast send and each fan-out leg of a broadcast rolls
/// independently.
const DROP_RATE: f64 = 0.1;

/// Wire messages between peers in the mock network. The sender's `PeerId`
/// is attached by the network layer; it is never carried in the payload.
#[derive(Debug, Clone, PartialEq, Eq, Encodable, Decodable)]
pub enum Message<D: UnitData> {
    /// Initial proposal from a unit's creator. Sent only by the unit's
    /// creator at creation time; `sig` is the creator's schnorr signature
    /// over the unit hash.
    Propose {
        /// The unit being proposed.
        unit: Unit<D>,
        /// The creator's schnorr signature over `unit.hash()`.
        sig: schnorr::Signature,
    },
    /// One peer's co-signature endorsing the unit at `(round, creator)`.
    /// The signer is implicit from the network layer; the signature
    /// verifies against the receiver's locally-held unit hash at that
    /// slot — a forker that split co-signers across two distinct units
    /// reaches threshold on neither side.
    Ack {
        /// Round of the unit being endorsed.
        round: Round,
        /// Creator of the unit being endorsed.
        creator: PeerId,
        /// The signer's schnorr signature over the unit's hash.
        sig: schnorr::Signature,
    },
    /// Announcement that the sender has locally observed the unit at
    /// `(unit.round, unit.creator)` cross the confirmation threshold.
    /// Carries the sender's full sig set so a peer that's missing acks
    /// (or missing the unit entirely) can catch up in one message.
    Confirmed {
        /// The confirmed unit.
        unit: Unit<D>,
        /// Co-signatures collected by the sender, keyed by signer.
        sigs: BTreeMap<PeerId, schnorr::Signature>,
    },
    /// Periodic anti-entropy summary: for each creator, the highest round
    /// the sender holds a unit at. Receivers compare this to their own
    /// state and unicast the gap (`Confirmed` for confirmed units, plain
    /// `Propose` otherwise) back to the sender for any creator where they
    /// have more.
    Status {
        /// Highest round the sender holds an entry at, keyed by creator.
        highest: BTreeMap<PeerId, Round>,
    },
}

/// Intended recipient of an [`INetwork::send`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recipient {
    /// Fan out to every peer except self.
    Everyone,
    /// A single peer (must not be self).
    Peer(PeerId),
}

/// `Arc`-erased [`INetwork`]. The engine consumes this so callers can swap
/// in any concrete implementation — a mock for tests, the daemon's
/// `ReconnectP2PConnections` for production.
pub type DynNetwork<M> = Arc<dyn INetwork<M>>;

/// Network layer the engine drives. Mirrors the upstream
/// `fedimint_core::net::peers::IP2PConnections<M>` shape so the same
/// implementation can later be reused by a DKG that wants per-peer
/// round-robin reads.
#[async_trait]
pub trait INetwork<M>: Send + Sync + 'static {
    /// Send `msg`. With [`Recipient::Everyone`] this fans out to every
    /// peer except self. The send is fire-and-forget — drops on full
    /// outbound queues, disconnected peers, or simulated packet loss are
    /// silently swallowed. The consensus layer is expected to retransmit.
    fn send(&self, recipient: Recipient, msg: M);

    /// Await the next inbound message from any peer. Returns `None` only
    /// after every sender has been dropped (i.e. the network is shutting
    /// down).
    async fn receive(&self) -> Option<(PeerId, M)>;

    /// Await the next inbound message from a specific peer. Used by the
    /// round-robin DKG step, not by the consensus engine. Implementations
    /// that don't have a DKG (e.g. the mock used in tests) may leave this
    /// as `unimplemented!()`.
    async fn receive_from_peer(&self, peer: PeerId) -> Option<M>;

    /// Wrap `self` into a [`DynNetwork`] for type erasure.
    fn into_dyn(self) -> DynNetwork<M>
    where
        Self: Sized,
    {
        Arc::new(self)
    }
}

/// Channel-backed mock network. Each peer holds one `MockChannel<D>`.
/// Built via [`MockChannel::mesh`] for an N-peer fully-connected mesh;
/// sends drop with probability `DROP_RATE` per recipient leg to simulate
/// an unreliable network. Implements [`INetwork<Message<D>>`].
pub struct MockChannel<D: UnitData> {
    own_id: PeerId,
    senders: BTreeMap<PeerId, Sender<(PeerId, Message<D>)>>,
    rx: Receiver<(PeerId, Message<D>)>,
}

impl<D: UnitData> MockChannel<D> {
    /// Build a fully-connected mesh of channels, one per peer in `n`.
    pub fn mesh(n: NumPeers) -> BTreeMap<PeerId, MockChannel<D>> {
        let mut receivers = BTreeMap::new();
        let mut senders = BTreeMap::new();

        for peer in n.peer_ids() {
            let (tx, rx) = async_channel::unbounded();
            senders.insert(peer, tx);
            receivers.insert(peer, rx);
        }

        n.peer_ids()
            .map(|own_id| {
                let rx = receivers.remove(&own_id).expect("inserted above");
                let channel = MockChannel {
                    own_id,
                    senders: senders.clone(),
                    rx,
                };
                (own_id, channel)
            })
            .collect()
    }
}

#[async_trait]
impl<D: UnitData> INetwork<Message<D>> for MockChannel<D> {
    fn send(&self, recipient: Recipient, msg: Message<D>) {
        match recipient {
            Recipient::Everyone => {
                for (peer, sender) in &self.senders {
                    if *peer == self.own_id {
                        continue;
                    }

                    if rand::random::<f64>() < DROP_RATE {
                        continue;
                    }

                    let _ = sender.try_send((self.own_id, msg.clone()));
                }
            }
            Recipient::Peer(to) => {
                assert_ne!(to, self.own_id, "MockChannel send must not target self");

                let sender = self
                    .senders
                    .get(&to)
                    .expect("recipient must be a known peer");

                if rand::random::<f64>() < DROP_RATE {
                    return;
                }

                let _ = sender.try_send((self.own_id, msg));
            }
        }
    }

    async fn receive(&self) -> Option<(PeerId, Message<D>)> {
        self.rx.recv().await.ok()
    }

    async fn receive_from_peer(&self, _peer: PeerId) -> Option<Message<D>> {
        unimplemented!(
            "MockChannel multiplexes inbound traffic on a single receiver; \
             per-peer reads are only meaningful for round-robin DKG, which \
             picomint-aleph-bft doesn't have"
        )
    }
}
