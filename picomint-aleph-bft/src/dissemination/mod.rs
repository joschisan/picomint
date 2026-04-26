use crate::{
    collection::{NewestUnitResponse, Salt},
    dag::Request as ReconstructionRequest,
    network::UnitMessage,
    units::UncheckedSignedUnit,
    Data, PeerId, Recipient, Signature, UncheckedSigned, UnitHash,
};

mod responder;
mod task;

pub use responder::Responder;
pub use task::{Manager as TaskManager, ManagerStatus as TaskManagerStatus};

const LOG_TARGET: &str = "AlephBFT-dissemination";

/// Some form of message with the intended recipients.
#[derive(Eq, PartialEq, Debug, Clone)]
pub struct Addressed<T> {
    message: T,
    recipients: Vec<Recipient>,
}

impl<T> Addressed<T> {
    /// Message with the given recipients.
    pub fn new(message: T, recipients: Vec<Recipient>) -> Self {
        Addressed {
            message,
            recipients,
        }
    }

    /// Message with the single specified recipient.
    pub fn addressed_to(message: T, node_id: PeerId) -> Self {
        Addressed::new(message, vec![Recipient::Peer(node_id)])
    }

    /// Message that should be broadcast.
    pub fn broadcast(message: T) -> Self {
        Addressed::new(message, vec![Recipient::Everyone])
    }

    /// All the recipients of this message.
    pub fn recipients(&self) -> &Vec<Recipient> {
        &self.recipients
    }

    /// The associated message.
    pub fn message(&self) -> &T {
        &self.message
    }

    /// Convert the underlying message. Cannot be done through a `From` implementation due to it
    /// overriding the blanked identity `From` implementation.
    pub fn into<U: From<T>>(self) -> Addressed<U> {
        let Addressed {
            message,
            recipients,
        } = self;
        Addressed {
            message: message.into(),
            recipients,
        }
    }
}

/// Responses to requests.
#[derive(Eq, PartialEq, Debug, Clone)]
pub enum DisseminationResponse<D: Data> {
    /// Response to a coord request, just a single unit.
    Coord(UncheckedSignedUnit<D>),
    /// All the parents of the specified unit.
    Parents(UnitHash, Vec<UncheckedSignedUnit<D>>),
    /// The newest unit response for initial unit collection.
    NewestUnit(UncheckedSigned<NewestUnitResponse<D>, Signature>),
}

/// A message that has to be passed between committee members for consensus to work.
#[derive(Eq, PartialEq, Debug, Clone)]
pub enum DisseminationMessage<D: Data> {
    /// Unit, either broadcast or in response to a coord request.
    Unit(UncheckedSignedUnit<D>),
    /// Request coming from the specified node for something.
    Request(PeerId, ReconstructionRequest),
    /// Response to a parent request.
    ParentsResponse(UnitHash, Vec<UncheckedSignedUnit<D>>),
    /// Initial unit collection request.
    NewestUnitRequest(PeerId, Salt),
    /// Response to initial unit collection.
    NewestUnitResponse(UncheckedSigned<NewestUnitResponse<D>, Signature>),
}

impl<D: Data> From<UnitMessage<D>> for DisseminationMessage<D> {
    fn from(message: UnitMessage<D>) -> Self {
        use DisseminationMessage::*;
        match message {
            UnitMessage::Unit(u) => Unit(u),
            UnitMessage::CoordRequest(node_id, coord) => {
                Request(node_id, ReconstructionRequest::Coord(coord))
            }
            UnitMessage::ParentsRequest(node_id, hash) => {
                Request(node_id, ReconstructionRequest::ParentsOf(hash))
            }
            UnitMessage::ParentsResponse(h, units) => ParentsResponse(h, units),
            UnitMessage::NewestRequest(node_id, salt) => NewestUnitRequest(node_id, salt),
            UnitMessage::NewestResponse(response) => NewestUnitResponse(response),
        }
    }
}

impl<D: Data> From<DisseminationMessage<D>> for UnitMessage<D> {
    fn from(message: DisseminationMessage<D>) -> Self {
        use DisseminationMessage::*;
        match message {
            Unit(u) => UnitMessage::Unit(u),
            Request(node_id, ReconstructionRequest::Coord(coord)) => {
                UnitMessage::CoordRequest(node_id, coord)
            }
            Request(node_id, ReconstructionRequest::ParentsOf(hash)) => {
                UnitMessage::ParentsRequest(node_id, hash)
            }
            ParentsResponse(h, units) => UnitMessage::ParentsResponse(h, units),
            NewestUnitRequest(node_id, salt) => UnitMessage::NewestRequest(node_id, salt),
            NewestUnitResponse(response) => UnitMessage::NewestResponse(response),
        }
    }
}

impl<D: Data> From<DisseminationResponse<D>> for DisseminationMessage<D> {
    fn from(message: DisseminationResponse<D>) -> Self {
        use DisseminationMessage::*;
        use DisseminationResponse::*;
        match message {
            Coord(u) => Unit(u),
            Parents(h, units) => ParentsResponse(h, units),
            NewestUnit(response) => NewestUnitResponse(response),
        }
    }
}
