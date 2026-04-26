use std::fmt::{Display, Formatter, Result as FmtResult};

use crate::{
    Data, Index, MultiKeychain, NumPeers, PeerId, Round, SessionId, Signable, Signed,
    UncheckedSigned, UnitHash,
};
use derivative::Derivative;
use picomint_encoding::{Decodable, Encodable};

mod control_hash;
mod store;
#[cfg(test)]
mod testing;
mod validator;

pub use control_hash::{ControlHash, Error as ControlHashError};
pub(crate) use store::*;
#[cfg(test)]
pub use testing::{
    create_preunits, creator_set, full_unit_to_unchecked_signed_unit,
    minimal_reconstructed_dag_units_up_to, preunit_to_full_unit, preunit_to_signed_unit,
    preunit_to_unchecked_signed_unit, random_full_parent_reconstrusted_units_up_to,
    random_full_parent_units_up_to, random_reconstructed_unit_with_parents,
    random_unit_with_parents, DagUnit as TestingDagUnit, FullUnit as TestingFullUnit,
    SignedUnit as TestingSignedUnit, WrappedSignedUnit,
};
pub use validator::{ValidationError, Validator};

/// The coordinates of a unit, i.e. creator and round. In the absence of forks this uniquely
/// determines a unit within a session.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Default, Encodable, Decodable)]
pub struct UnitCoord {
    round: Round,
    creator: PeerId,
}

impl UnitCoord {
    pub fn new(round: Round, creator: PeerId) -> Self {
        Self { creator, round }
    }

    pub fn creator(&self) -> PeerId {
        self.creator
    }

    pub fn round(&self) -> Round {
        self.round
    }
}

impl Display for UnitCoord {
    fn fmt(&self, f: &mut Formatter) -> FmtResult {
        write!(f, "(#{} by {})", self.round, self.creator.to_usize())
    }
}

/// The simplest type representing a unit, consisting of coordinates and a control hash
#[derive(Clone, Eq, PartialEq, Hash, Debug, Decodable, Encodable)]
pub struct PreUnit {
    coord: UnitCoord,
    control_hash: ControlHash,
}

impl PreUnit {
    pub(crate) fn new(creator: PeerId, round: Round, control_hash: ControlHash) -> Self {
        PreUnit {
            coord: UnitCoord::new(round, creator),
            control_hash,
        }
    }

    pub(crate) fn n_members(&self) -> NumPeers {
        self.control_hash.n_members()
    }

    pub(crate) fn creator(&self) -> PeerId {
        self.coord.creator()
    }

    pub(crate) fn round(&self) -> Round {
        self.coord.round()
    }

    pub(crate) fn control_hash(&self) -> &ControlHash {
        &self.control_hash
    }
}

#[derive(Clone, Debug, Decodable, Derivative, Encodable)]
#[derivative(Eq, PartialEq, Hash)]
pub struct FullUnit<D: Data> {
    pre_unit: PreUnit,
    data: Option<D>,
    session_id: SessionId,
}

impl<D: Data> From<FullUnit<D>> for Option<D> {
    fn from(value: FullUnit<D>) -> Self {
        value.data
    }
}

impl<D: Data> FullUnit<D> {
    pub(crate) fn new(pre_unit: PreUnit, data: Option<D>, session_id: SessionId) -> Self {
        FullUnit {
            pre_unit,
            data,
            session_id,
        }
    }
    pub(crate) fn as_pre_unit(&self) -> &PreUnit {
        &self.pre_unit
    }
    pub(crate) fn data(&self) -> &Option<D> {
        &self.data
    }
    pub(crate) fn included_data(&self) -> Vec<D> {
        self.data.iter().cloned().collect()
    }
}

impl<D: Data> Signable for FullUnit<D> {
    type Hash = UnitHash;
    fn hash(&self) -> UnitHash {
        Unit::hash(self)
    }
}

impl<D: Data> Index for FullUnit<D> {
    fn index(&self) -> PeerId {
        self.creator()
    }
}

pub(crate) type UncheckedSignedUnit<D, S> = UncheckedSigned<FullUnit<D>, S>;

pub(crate) type SignedUnit<D, K> = Signed<FullUnit<D>, K>;

/// Abstract representation of a unit from the Dag point of view.
pub trait Unit: 'static + Send + Clone {
    fn hash(&self) -> UnitHash;

    fn coord(&self) -> UnitCoord;

    fn control_hash(&self) -> &ControlHash;

    fn session_id(&self) -> SessionId;

    fn creator(&self) -> PeerId {
        self.coord().creator()
    }

    fn round(&self) -> Round {
        self.coord().round()
    }
}

pub trait WrappedUnit: Unit {
    type Wrapped: Unit;

    fn unpack(self) -> Self::Wrapped;
}

pub trait UnitWithParents: Unit {
    fn parents(&self) -> impl Iterator<Item = &UnitHash>;
    fn direct_parents(&self) -> impl Iterator<Item = &UnitHash>;
    fn parent_for(&self, index: PeerId) -> Option<&UnitHash>;

    fn node_count(&self) -> NumPeers;
}

impl<D: Data> Unit for FullUnit<D> {
    fn hash(&self) -> UnitHash {
        crate::hash(&self.consensus_encode_to_vec())
    }

    fn coord(&self) -> UnitCoord {
        self.pre_unit.coord
    }

    fn control_hash(&self) -> &ControlHash {
        self.pre_unit.control_hash()
    }

    fn session_id(&self) -> SessionId {
        self.session_id
    }
}

impl<D: Data, MK: MultiKeychain> Unit for SignedUnit<D, MK> {
    fn hash(&self) -> UnitHash {
        Unit::hash(self.as_signable())
    }

    fn coord(&self) -> UnitCoord {
        self.as_signable().coord()
    }

    fn control_hash(&self) -> &ControlHash {
        self.as_signable().control_hash()
    }

    fn session_id(&self) -> SessionId {
        self.as_signable().session_id()
    }
}

#[cfg(test)]
pub mod tests {
    use crate::{
        units::{random_full_parent_units_up_to, FullUnit, Unit},
        NumPeers,
    };
    use aleph_bft_mock::Data;
    use picomint_encoding::{Decodable, Encodable};

    pub type TestFullUnit = FullUnit<Data>;

    #[test]
    fn test_full_unit_hash_is_correct() {
        for full_unit in random_full_parent_units_up_to(3, NumPeers::new(4 as usize), 43)
            .into_iter()
            .flatten()
        {
            let hash = crate::hash(&full_unit.consensus_encode_to_vec());
            assert_eq!(full_unit.hash(), hash);
        }
    }

    #[test]
    fn test_full_unit_codec() {
        for full_unit in random_full_parent_units_up_to(3, NumPeers::new(4 as usize), 43)
            .into_iter()
            .flatten()
        {
            let encoded = full_unit.consensus_encode_to_vec();
            let decoded = TestFullUnit::consensus_decode_partial(&mut encoded.as_slice())
                .expect("should decode correctly");
            assert_eq!(decoded, full_unit);
        }
    }
}
