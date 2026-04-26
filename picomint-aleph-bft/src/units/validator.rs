use crate::units::{ControlHashError, UnitCoord};
use crate::{
    units::{FullUnit, PreUnit, SignedUnit, UncheckedSignedUnit, Unit},
    Data, Keychain, NumPeers, PeerId, Round, SessionId, Signature, SignatureError,
};
use std::{
    fmt::{Display, Formatter, Result as FmtResult},
    result::Result as StdResult,
};

/// All that can be wrong with a unit except control hash issues.
#[derive(Eq, PartialEq, Debug)]
pub enum ValidationError<D: Data> {
    WrongSignature(UncheckedSignedUnit<D>),
    WrongSession(FullUnit<D>),
    RoundTooHigh(FullUnit<D>),
    WrongNumberOfMembers(PreUnit),
    ParentValidationFailed(PreUnit, ControlHashError),
}

impl<D: Data> Display for ValidationError<D> {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        use ValidationError::*;
        match self {
            WrongSignature(usu) => write!(f, "wrongly signed unit: {:?}", usu),
            WrongSession(fu) => write!(f, "unit from wrong session: {:?}", fu),
            RoundTooHigh(fu) => write!(f, "unit with too high round {}: {:?}", fu.round(), fu),
            WrongNumberOfMembers(pu) => write!(
                f,
                "wrong number of members implied by unit {:?}: {:?}",
                pu.n_members(),
                pu
            ),
            ParentValidationFailed(pu, control_hash_error) => write!(
                f,
                "parent validation failed for unit: {:?}. Internal error: {}",
                pu, control_hash_error
            ),
        }
    }
}

impl<D: Data> From<SignatureError<FullUnit<D>, Signature>> for ValidationError<D> {
    fn from(se: SignatureError<FullUnit<D>, Signature>) -> Self {
        ValidationError::WrongSignature(se.unchecked)
    }
}

#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Validator {
    session_id: SessionId,
    keychain: Keychain,
    max_round: Round,
}

type Result<D> = StdResult<SignedUnit<D>, ValidationError<D>>;

impl Validator {
    pub fn new(session_id: SessionId, keychain: Keychain, max_round: Round) -> Self {
        Validator {
            session_id,
            keychain,
            max_round,
        }
    }

    pub fn node_count(&self) -> NumPeers {
        self.keychain.node_count()
    }

    pub fn index(&self) -> PeerId {
        self.keychain.identity()
    }

    #[allow(clippy::result_large_err)]
    pub fn validate_unit<D: Data>(&self, uu: UncheckedSignedUnit<D>) -> Result<D> {
        let su = uu.check(&self.keychain)?;
        let full_unit = su.as_signable();
        if full_unit.session_id() != self.session_id {
            return Err(ValidationError::WrongSession(full_unit.clone()));
        }
        if full_unit.round() > self.max_round {
            return Err(ValidationError::RoundTooHigh(full_unit.clone()));
        }
        self.validate_unit_parents(su)
    }

    #[allow(clippy::result_large_err)]
    fn validate_unit_parents<D: Data>(&self, su: SignedUnit<D>) -> Result<D> {
        let pre_unit = su.as_signable().as_pre_unit();
        let n_members = pre_unit.n_members();
        if n_members != self.keychain.node_count() {
            return Err(ValidationError::WrongNumberOfMembers(pre_unit.clone()));
        }
        let unit_coord = UnitCoord::new(pre_unit.round(), pre_unit.creator());
        pre_unit
            .control_hash
            .validate(unit_coord)
            .map_err(|e| ValidationError::ParentValidationFailed(pre_unit.clone(), e))?;
        Ok(su)
    }
}

#[cfg(test)]
mod tests {
    use super::{ValidationError::*, Validator};
    use crate::{
        units::{
            full_unit_to_unchecked_signed_unit, preunit_to_unchecked_signed_unit,
            random_full_parent_units_up_to, random_unit_with_parents, PreUnit,
            {ControlHash, ControlHashError},
        },
        NumPeers, PeerId,
    };
    use aleph_bft_mock::keychain;
    use picomint_encoding::{Decodable, Encodable};

    #[test]
    fn validates_initial_unit() {
        let n_members = NumPeers::new(7 as usize);
        let creator_id = PeerId::new(0 as u8);
        let session_id = 0;
        let max_round = 2;
        let kc = keychain(n_members, creator_id);
        let validator = Validator::new(session_id, kc.clone(), max_round);
        let full_unit = random_full_parent_units_up_to(0, n_members, session_id)[0][0].clone();
        let unchecked_unit = full_unit_to_unchecked_signed_unit(full_unit, &kc);
        let checked_unit = validator
            .validate_unit(unchecked_unit.clone())
            .expect("Unit should validate.");
        assert_eq!(unchecked_unit, checked_unit.into());
    }

    #[test]
    fn detects_wrong_initial_control_hash() {
        let n_members = NumPeers::new(7 as usize);
        let creator_id = PeerId::new(0 as u8);
        let session_id = 0;
        let max_round = 2;
        let kc = keychain(n_members, creator_id);
        let validator = Validator::new(session_id, kc.clone(), max_round);
        let preunit = random_full_parent_units_up_to(0, n_members, session_id)[0][0]
            .as_pre_unit()
            .clone();
        let mut control_hash = preunit.control_hash().clone();
        let encoded = control_hash.consensus_encode_to_vec();
        let prefix_len = encoded.len() - 32;
        let mut borked_control_hash_bytes = encoded[..prefix_len].to_vec();
        borked_control_hash_bytes.extend([0u8; 32]);
        control_hash =
            ControlHash::consensus_decode_partial(&mut borked_control_hash_bytes.as_slice())
                .expect("should decode correctly");
        let preunit = PreUnit::new(preunit.creator(), preunit.round(), control_hash);
        let unchecked_unit = preunit_to_unchecked_signed_unit(preunit.clone(), session_id, &kc);
        let other_preunit = match validator.validate_unit(unchecked_unit.clone()) {
            Ok(_) => panic!("Validated bad unit."),
            Err(ParentValidationFailed(unit, ControlHashError::RoundZeroBadControlHash(_, _))) => {
                unit
            }
            Err(e) => panic!("Unexpected error from validator: {:?}", e),
        };
        assert_eq!(other_preunit, preunit);
    }

    #[test]
    fn detects_wrong_session_id() {
        let n_members = NumPeers::new(7 as usize);
        let creator_id = PeerId::new(0 as u8);
        let session_id = 0;
        let wrong_session_id = 43;
        let max_round = 2;
        let kc = keychain(n_members, creator_id);
        let validator = Validator::new(session_id, kc.clone(), max_round);
        let full_unit =
            random_full_parent_units_up_to(0, n_members, wrong_session_id)[0][0].clone();
        let unchecked_unit = full_unit_to_unchecked_signed_unit(full_unit, &kc);
        let full_unit = match validator.validate_unit(unchecked_unit.clone()) {
            Ok(_) => panic!("Validated bad unit."),
            Err(WrongSession(full_unit)) => full_unit,
            Err(e) => panic!("Unexpected error from validator: {:?}", e),
        };
        assert_eq!(full_unit, unchecked_unit.into_signable());
    }

    #[test]
    fn detects_wrong_number_of_members() {
        let n_members = NumPeers::new(7 as usize);
        let n_plus_one_members = NumPeers::new(8 as usize);
        let creator_id = PeerId::new(0 as u8);
        let session_id = 0;
        let max_round = 2;
        let kc = keychain(n_plus_one_members, creator_id);
        let validator = Validator::new(session_id, kc.clone(), max_round);
        let full_unit = random_full_parent_units_up_to(0, n_members, session_id)[0][0].clone();
        let preunit = full_unit.as_pre_unit().clone();
        let unchecked_unit = full_unit_to_unchecked_signed_unit(full_unit, &kc);
        let other_preunit = match validator.validate_unit(unchecked_unit) {
            Ok(_) => panic!("Validated bad unit."),
            Err(WrongNumberOfMembers(other_preunit)) => other_preunit,
            Err(e) => panic!("Unexpected error from validator: {:?}", e),
        };
        assert_eq!(other_preunit, preunit);
    }

    #[test]
    fn detects_below_threshold() {
        let n_members = NumPeers::new(7 as usize);
        let creator_id = PeerId::new(0 as u8);
        let session_id = 0;
        let max_round = 2;
        let parents = random_full_parent_units_up_to(0, n_members, session_id)[0]
            .iter()
            .take(4)
            .cloned()
            .collect();
        let unit = random_unit_with_parents(creator_id, &parents, 1);
        let preunit = unit.as_pre_unit().clone();
        let kc = keychain(n_members, creator_id);
        let unchecked_unit = full_unit_to_unchecked_signed_unit(unit, &kc);
        let validator = Validator::new(session_id, kc, max_round);
        let other_preunit = match validator.validate_unit(unchecked_unit) {
            Ok(_) => panic!("Validated bad unit."),
            Err(ParentValidationFailed(
                other_preunit,
                ControlHashError::NotEnoughParentsForRound(_),
            )) => other_preunit,
            Err(e) => panic!("Unexpected error from validator: {:?}", e),
        };
        assert_eq!(other_preunit, preunit);
    }

    #[test]
    fn detects_too_high_round() {
        let n_members = NumPeers::new(7 as usize);
        let creator_id = PeerId::new(0 as u8);
        let session_id = 0;
        let max_round = 2;
        let kc = keychain(n_members, creator_id);
        let validator = Validator::new(session_id, kc.clone(), max_round);
        let full_unit = random_full_parent_units_up_to(3, n_members, session_id)[3][0].clone();
        let unchecked_unit = full_unit_to_unchecked_signed_unit(full_unit, &kc);
        let full_unit = match validator.validate_unit(unchecked_unit.clone()) {
            Ok(_) => panic!("Validated bad unit."),
            Err(RoundTooHigh(full_unit)) => full_unit,
            Err(e) => panic!("Unexpected error from validator: {:?}", e),
        };
        assert_eq!(full_unit, unchecked_unit.into_signable());
    }
}
