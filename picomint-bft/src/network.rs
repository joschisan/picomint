use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_channel::{Receiver, Sender};
use async_trait::async_trait;
use picomint_core::secp256k1::schnorr;
use picomint_core::{NumPeers, PeerId};
use picomint_encoding::{Decodable, Encodable};
use rand::Rng;

use crate::unit::{Round, Unit, UnitData};

/// Per-recipient probability of silently dropping a message in the mock
/// network. Each unicast send and each fan-out leg of a broadcast rolls
/// independently.
const DROP_RATE: f64 = 0.05;

/// Base one-way latency applied to every delivered message in the mock
/// network. Each send adds `BASE_LATENCY` plus a uniform jitter in
/// `[0, JITTER]` before the message lands in the recipient's inbox.
const BASE_LATENCY: Duration = Duration::from_millis(25);
const JITTER: Duration = Duration::from_millis(15);

/// Wire messages between peers. The sender's `PeerId` is attached by the
/// network layer; it is never carried in the payload.
#[derive(Debug, Clone, PartialEq, Eq, Encodable, Decodable)]
pub enum Message<D: UnitData> {
    /// Unified unit dissemination. Carries the unit body plus a subset
    /// (1..=threshold) of the co-signatures over it. Receivers union the
    /// carried sigs with what they already hold — duplicate body and
    /// duplicate sigs are no-ops. The bundle must contain a sig from the
    /// unit's creator (binds the body to its claimed author so a Byzantine
    /// peer can't fabricate a body at someone else's slot).
    Unit {
        /// The unit being disseminated.
        unit: Unit<D>,
        /// Co-signatures over `unit`, keyed by signer. Always includes
        /// the creator's sig.
        sigs: BTreeMap<PeerId, schnorr::Signature>,
    },
    /// Targeted backfill request. Sent every anti-entropy cycle for the
    /// requester's lowest unconfirmed `(round, creator)` slot per peer;
    /// the recipient replies with a `Unit` carrying its view of the slot
    /// (body plus all sigs it currently holds) if it has the entry.
    Request {
        /// Round of the slot the requester wants filled.
        round: Round,
        /// Creator of the slot the requester wants filled.
        creator: PeerId,
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

fn delayed_send<D: UnitData>(sender: Sender<(PeerId, Message<D>)>, from: PeerId, msg: Message<D>) {
    let jitter = Duration::from_micros(rand::thread_rng().gen_range(0..=JITTER.as_micros() as u64));
    let delay = BASE_LATENCY + jitter;

    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        let _ = sender.try_send((from, msg));
    });
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

                    delayed_send(sender.clone(), self.own_id, msg.clone());
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

                delayed_send(sender.clone(), self.own_id, msg);
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
             picomint-bft doesn't have"
        )
    }
}
