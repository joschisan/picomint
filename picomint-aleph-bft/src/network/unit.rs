use crate::{
    collection::NewestUnitResponse,
    units::{UncheckedSignedUnit, UnitCoord},
    Data, PeerId, Signature, UncheckedSigned, UnitHash,
};
use picomint_encoding::{Decodable, Encodable};

/// A message concerning units, either about new units or some requests for them.
#[derive(Clone, Eq, PartialEq, Debug, Decodable, Encodable)]
pub enum UnitMessage<D: Data> {
    /// For disseminating newly created units.
    Unit(UncheckedSignedUnit<D>),
    /// Request for a unit by its coord.
    CoordRequest(PeerId, UnitCoord),
    /// Request for the full list of parents of a unit.
    ParentsRequest(PeerId, UnitHash),
    /// Response to a request for a full list of parents.
    ParentsResponse(UnitHash, Vec<UncheckedSignedUnit<D>>),
    /// Request by a node for the newest unit created by them, together with a u64 salt
    NewestRequest(PeerId, u64),
    /// Response to RequestNewest: (our index, maybe unit, salt) signed by us
    NewestResponse(UncheckedSigned<NewestUnitResponse<D>, Signature>),
}

impl<D: Data> UnitMessage<D> {
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
