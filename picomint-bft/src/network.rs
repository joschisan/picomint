use std::sync::Arc;

use async_trait::async_trait;
use picomint_core::PeerId;
use picomint_encoding::{Decodable, Encodable};

use crate::unit::{Round, SignedUnit, UnitData};

/// Wire messages between peers. See `README.md` for the protocol;
/// the sender's `PeerId` is attached by the network layer.
#[derive(Debug, Clone, PartialEq, Eq, Encodable, Decodable)]
pub enum Message<D: UnitData> {
    /// Body + creator sig. Emitted by the creator on creation, by the
    /// creator's anti-entropy push of its own column, and as the sole
    /// `Request` response.
    Unit(SignedUnit<D>),
    /// Targeted backfill. The recipient replies with `Unit` if it
    /// holds the slot; otherwise no reply.
    Request { round: Round, creator: PeerId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recipient {
    /// Fan out to every peer except self.
    Everyone,
    /// A single peer (must not be self).
    Peer(PeerId),
}

pub type DynNetwork<D> = Arc<dyn INetwork<D>>;

/// Engine's network surface. Shape mirrors fedimint's
/// `IP2PConnections<M>` so it can be reused by a future DKG that wants
/// per-peer round-robin reads.
#[async_trait]
pub trait INetwork<D: UnitData>: Send + Sync + 'static {
    /// Fire-and-forget. Drops are silently swallowed; the consensus
    /// layer retransmits.
    fn send(&self, recipient: Recipient, msg: Message<D>);

    /// `None` once every sender has been dropped.
    async fn receive(&self) -> Option<(PeerId, Message<D>)>;

    /// Per-peer read for round-robin DKG. Mocks may leave this as
    /// `unimplemented!()`.
    async fn receive_from_peer(&self, peer: PeerId) -> Option<Message<D>>;

    fn into_dyn(self) -> DynNetwork<D>
    where
        Self: Sized,
    {
        Arc::new(self)
    }
}
