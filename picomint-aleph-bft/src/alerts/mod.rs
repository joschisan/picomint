use crate::{
    units::{UncheckedSignedUnit, Unit},
    Data, Index, Keychain, MultiKeychain, NodeIndex, PartialMultisignature, Signable, Signature,
    UncheckedSigned, UnitHash,
};
use aleph_bft_rmc::Message as RmcMessage;
use codec::{Decode, Encode};
use derivative::Derivative;

mod handler;
mod service;

pub use handler::Handler;
pub use service::{Service, IO};

pub type ForkProof<D, S> = (UncheckedSignedUnit<D, S>, UncheckedSignedUnit<D, S>);

pub type NetworkMessage<D, MK> =
    AlertMessage<D, <MK as Keychain>::Signature, <MK as MultiKeychain>::PartialMultisignature>;

#[derive(Clone, Debug, Decode, Derivative, Encode)]
#[derivative(Eq, PartialEq, Hash)]
pub struct Alert<D: Data, S: Signature> {
    sender: NodeIndex,
    proof: ForkProof<D, S>,
    legit_units: Vec<UncheckedSignedUnit<D, S>>,
}

impl<D: Data, S: Signature> Alert<D, S> {
    pub fn new(
        sender: NodeIndex,
        proof: ForkProof<D, S>,
        legit_units: Vec<UncheckedSignedUnit<D, S>>,
    ) -> Alert<D, S> {
        Alert {
            sender,
            proof,
            legit_units,
        }
    }

    /// Simplified forker check, should only be called for alerts that have already been checked to
    /// contain valid proofs.
    pub fn forker(&self) -> NodeIndex {
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

impl<D: Data, S: Signature> Index for Alert<D, S> {
    fn index(&self) -> NodeIndex {
        self.sender
    }
}

impl<D: Data, S: Signature> Signable for Alert<D, S> {
    type Hash = UnitHash;
    fn hash(&self) -> Self::Hash {
        self.using_encoded(crate::hash)
    }
}

/// A message concerning alerts.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Decode, Encode)]
#[allow(clippy::large_enum_variant)]
pub enum AlertMessage<D: Data, S: Signature, MS: PartialMultisignature> {
    /// Alert regarding forks, signed by the person claiming misconduct.
    ForkAlert(UncheckedSigned<Alert<D, S>, S>),
    /// An internal RMC message, together with the id of the sender.
    RmcMessage(NodeIndex, RmcMessage<UnitHash, S, MS>),
    /// A request by a node for a fork alert identified by the given hash.
    AlertRequest(NodeIndex, UnitHash),
}

impl<D: Data, S: Signature, MS: PartialMultisignature> AlertMessage<D, S, MS> {
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
#[derive(Clone, Eq, PartialEq, Hash, Debug, Decode, Encode)]
#[allow(clippy::large_enum_variant)]
pub enum ForkingNotification<D: Data, S: Signature> {
    Forker(ForkProof<D, S>),
    Units(Vec<UncheckedSignedUnit<D, S>>),
}
