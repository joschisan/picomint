use crate::{
    collection::NewestUnitResponse,
    units::{UncheckedSignedUnit, UnitCoord},
    Data, NodeIndex, Signature, UncheckedSigned, UnitHash,
};
use codec::{Decode, Encode};

/// A message concerning units, either about new units or some requests for them.
#[derive(Clone, Eq, PartialEq, Debug, Decode, Encode)]
pub enum UnitMessage<D: Data, S: Signature> {
    /// For disseminating newly created units.
    Unit(UncheckedSignedUnit<D, S>),
    /// Request for a unit by its coord.
    CoordRequest(NodeIndex, UnitCoord),
    /// Request for the full list of parents of a unit.
    ParentsRequest(NodeIndex, UnitHash),
    /// Response to a request for a full list of parents.
    ParentsResponse(UnitHash, Vec<UncheckedSignedUnit<D, S>>),
    /// Request by a node for the newest unit created by them, together with a u64 salt
    NewestRequest(NodeIndex, u64),
    /// Response to RequestNewest: (our index, maybe unit, salt) signed by us
    NewestResponse(UncheckedSigned<NewestUnitResponse<D, S>, S>),
}

impl<D: Data, S: Signature> UnitMessage<D, S> {
    pub fn included_data(&self) -> Vec<D> {
        use UnitMessage::*;
        match self {
            Unit(uu) => uu.as_signable().included_data(),
            ParentsResponse(_, units) => units
                .iter()
                .flat_map(|uu| uu.as_signable().included_data())
                .collect(),
            NewestResponse(response) => response.as_signable().included_data(),
            NewestRequest(_, _) | CoordRequest(_, _) | ParentsRequest(_, _) => Vec::new(),
        }
    }
}
