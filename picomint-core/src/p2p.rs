use crate::PeerId;

/// The intended recipient of a peer-to-peer message: either a single peer or
/// every peer in the federation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recipient {
    Everyone,
    Peer(PeerId),
}
