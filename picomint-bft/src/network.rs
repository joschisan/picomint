use std::sync::Arc;

use async_trait::async_trait;
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

pub type DynNetwork<D> = Arc<dyn INetwork<D>>;

/// Engine's network surface. Shape mirrors fedimint's
/// `IP2PConnections<M>` so it can be reused by a future DKG that wants
/// per-node round-robin reads.
#[async_trait]
pub trait INetwork<D: UnitData>: Send + Sync + 'static {
    /// Fire-and-forget. Drops are silently swallowed; the consensus
    /// layer retransmits.
    fn send(&self, recipient: Recipient, msg: Message<D>);

    /// `None` once every sender has been dropped.
    async fn receive(&self) -> Option<(NodeId, Message<D>)>;

    /// Per-node read for round-robin DKG. Mocks may leave this as
    /// `unimplemented!()`.
    async fn receive_from_node(&self, node: NodeId) -> Option<Message<D>>;

    fn into_dyn(self) -> DynNetwork<D>
    where
        Self: Sized,
    {
        Arc::new(self)
    }
}
