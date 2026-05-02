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
    /// Body dissemination. Carries the body plus *only* the creator's
    /// sig — cosigs never ride on `Unit`. Three paths emit one:
    /// 1. The creator's own broadcast at unit-creation time.
    /// 2. The creator's own anti-entropy push of its highest own slot.
    /// 3. A `Request` response (paired with one `Sig` per held cosig).
    /// Binding the body to its claimed creator via `creator_sig` is
    /// what blocks a Byzantine peer from fabricating a body at someone
    /// else's slot. Cosig propagation is the `Sig` message's job.
    Unit {
        /// The unit being disseminated.
        unit: Unit<D>,
        /// The creator's schnorr signature over `unit`'s consensus
        /// encoding. Verified against `unit.creator`'s public key.
        sig: schnorr::Signature,
    },
    /// Cosign-only fan-out. When a peer first cosigns a unit body it
    /// has received, it broadcasts a `Cosig` so every other peer can
    /// union it into their copy of the slot. The body is *not* carried
    /// — receivers either already hold it (then `record_cosig` against
    /// the local body), or pull it from the signer via `Request`.
    Cosig {
        /// Round of the slot being cosigned.
        round: Round,
        /// Creator of the slot being cosigned.
        creator: PeerId,
        /// The peer whose cosig this is. Always non-creator: the
        /// creator's signature lives in `Unit.sig`.
        signer: PeerId,
        /// Schnorr cosignature over the unit's consensus encoding.
        sig: schnorr::Signature,
    },
    /// Targeted backfill request. If the recipient holds the entry,
    /// it replies with a `Unit` carrying body + creator sig, plus one
    /// `Sig` per non-creator cosig it currently holds — so a single
    /// `Request` recovers both the body and all known cosigs in one
    /// round-trip.
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
