use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use picomint_core::PeerId;
use picomint_core::secp256k1::schnorr;
use picomint_encoding::{Decodable, Encodable};

use crate::unit::{Round, Unit, UnitData};

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
