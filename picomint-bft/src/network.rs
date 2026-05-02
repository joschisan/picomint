use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use picomint_core::PeerId;
use picomint_core::secp256k1::schnorr;
use picomint_encoding::{Decodable, Encodable};

use crate::unit::{Round, Unit, UnitData};

/// Wire messages between peers. See `README.md` for the protocol;
/// the sender's `PeerId` is attached by the network layer.
#[derive(Debug, Clone, PartialEq, Eq, Encodable, Decodable)]
pub enum Message<D: UnitData> {
    /// Body + creator sig. Emitted by the creator on creation, by the
    /// creator's anti-entropy push of its own column, and never as a
    /// `Request` response.
    Unit {
        unit: Unit<D>,
        sig: schnorr::Signature,
    },
    /// One cosignature, broadcast by the signer the first time they
    /// cosign the slot. Receivers without the body demand-pull it from
    /// the signer.
    Cosig {
        round: Round,
        creator: PeerId,
        signer: PeerId,
        sig: schnorr::Signature,
    },
    /// Threshold-proven slot view: body + creator sig + exactly `2f`
    /// cosigs. Sole `Request` response, and only emitted when the
    /// responder holds the slot at threshold locally. Receivers
    /// atomically install or overwrite their entry — quorum math
    /// forbids two distinct bodies reaching threshold, so a valid
    /// `SignedUnit` proves canonical body.
    SignedUnit {
        unit: Unit<D>,
        sig: schnorr::Signature,
        cosigs: BTreeMap<PeerId, schnorr::Signature>,
    },
    /// Targeted backfill. The recipient replies with `SignedUnit` if
    /// the slot is locally confirmed; otherwise no reply.
    Request { round: Round, creator: PeerId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recipient {
    /// Fan out to every peer except self.
    Everyone,
    /// A single peer (must not be self).
    Peer(PeerId),
}

pub type DynNetwork<M> = Arc<dyn INetwork<M>>;

/// Engine's network surface. Shape mirrors fedimint's
/// `IP2PConnections<M>` so it can be reused by a future DKG that wants
/// per-peer round-robin reads.
#[async_trait]
pub trait INetwork<M>: Send + Sync + 'static {
    /// Fire-and-forget. Drops are silently swallowed; the consensus
    /// layer retransmits.
    fn send(&self, recipient: Recipient, msg: M);

    /// `None` once every sender has been dropped.
    async fn receive(&self) -> Option<(PeerId, M)>;

    /// Per-peer read for round-robin DKG. Mocks may leave this as
    /// `unimplemented!()`.
    async fn receive_from_peer(&self, peer: PeerId) -> Option<M>;

    fn into_dyn(self) -> DynNetwork<M>
    where
        Self: Sized,
    {
        Arc::new(self)
    }
}
