use crate::{
    units::{UncheckedSignedUnit, Unit},
    Data, Index, PeerId, Signable, Signature, UncheckedSigned, UnitHash,
};
use aleph_bft_rmc::Message as RmcMessage;
use derivative::Derivative;
use picomint_encoding::{Decodable, Encodable};

mod handler;
mod service;

pub use handler::Handler;
pub use service::{Service, IO};

pub type ForkProof<D> = (UncheckedSignedUnit<D>, UncheckedSignedUnit<D>);

pub type NetworkMessage<D> = AlertMessage<D>;

#[derive(Clone, Debug, Decodable, Derivative, Encodable)]
#[derivative(Eq, PartialEq, Hash)]
pub struct Alert<D: Data> {
    sender: PeerId,
    proof: ForkProof<D>,
    legit_units: Vec<UncheckedSignedUnit<D>>,
}

impl<D: Data> Alert<D> {
    pub fn new(
        sender: PeerId,
        proof: ForkProof<D>,
        legit_units: Vec<UncheckedSignedUnit<D>>,
    ) -> Alert<D> {
        Alert {
            sender,
            proof,
            legit_units,
        }
    }

    /// Simplified forker check, should only be called for alerts that have already been checked to
    /// contain valid proofs.
    pub fn forker(&self) -> PeerId {
        self.proof.0.as_signable().creator()
    }

    pub fn included_data(&self) -> Vec<D> {
        // Only legit units might end up in the DAG, we can ignore the fork proof.
        self.legit_units
            .iter()
            .filter_map(|uu| uu.as_signable().data().clone())
            .collect()
    }
}

impl<D: Data> Index for Alert<D> {
    fn index(&self) -> PeerId {
        self.sender
    }
}

impl<D: Data> Signable for Alert<D> {
    type Hash = UnitHash;
    fn hash(&self) -> Self::Hash {
        crate::hash(&self.consensus_encode_to_vec())
    }
}

/// A message concerning alerts.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Decodable, Encodable)]
#[allow(clippy::large_enum_variant)]
pub enum AlertMessage<D: Data> {
    /// Alert regarding forks, signed by the person claiming misconduct.
    ForkAlert(UncheckedSigned<Alert<D>, Signature>),
    /// An internal RMC message, together with the id of the sender.
    RmcMessage(PeerId, RmcMessage<UnitHash>),
    /// A request by a node for a fork alert identified by the given hash.
    AlertRequest(PeerId, UnitHash),
}

impl<D: Data> AlertMessage<D> {
    pub fn included_data(&self) -> Vec<D> {
        match self {
            Self::ForkAlert(unchecked_alert) => unchecked_alert.as_signable().included_data(),
            Self::RmcMessage(_, _) => Vec::new(),
            Self::AlertRequest(_, _) => Vec::new(),
        }
    }
}

// Notifications being sent to consensus, so that it can learn about proven forkers and receive
// legitimized units.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Decodable, Encodable)]
#[allow(clippy::large_enum_variant)]
pub enum ForkingNotification<D: Data> {
    Forker(ForkProof<D>),
    Units(Vec<UncheckedSignedUnit<D>>),
}
