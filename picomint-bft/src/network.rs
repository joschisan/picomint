use std::future::Future;

use picomint_core::NodeId;
use picomint_encoding::{Decodable, Encodable};

use crate::unit::{UnitData, UnitEnvelope, UnitHash};

/// Wire messages between nodes. See `README.md` for the protocol;
/// the sender's `NodeId` is attached by the network layer.
#[derive(Debug, Clone, PartialEq, Eq, Encodable, Decodable)]
pub enum Message<D: UnitData> {
    /// Body + creator sig. Emitted by the creator on creation, by the
    /// creator's anti-entropy push of its own column, and as the sole
    /// `Request` response.
    Unit(UnitEnvelope<D>),
    /// Targeted backfill of the exact unit pinned by the hash. The
    /// recipient replies with `Unit` if it holds the envelope; otherwise
    /// no reply.
    Request(UnitHash),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recipient {
    /// Fan out to every node except self.
    Everyone,
    /// A single node (must not be self).
    Node(NodeId),
}

/// Engine's network surface.
pub trait INetwork<D: UnitData>: Send + Sync + 'static {
    /// Fire-and-forget. Drops are silently swallowed; the consensus
    /// layer retransmits.
    fn send(&self, recipient: Recipient, msg: Message<D>);

    /// `None` once every sender has been dropped.
    fn receive(&self) -> impl Future<Output = Option<(NodeId, Message<D>)>> + Send;
}
