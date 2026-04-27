use std::collections::BTreeSet;
use std::fmt::{Debug, Display, Formatter, Result as FmtResult};

use crate::{
    alerts::Alert,
    units::{
        SignedUnit, UncheckedSignedUnit, Unit, UnitStore, UnitStoreStatus, ValidationError,
        Validator as UnitValidator, WrappedUnit,
    },
    Data, PeerId, Round, UnitHash,
};

/// What can go wrong when validating a unit.
#[derive(Eq, PartialEq)]
pub enum Error<D: Data> {
    Invalid(ValidationError<D>),
    Duplicate(SignedUnit<D>),
    Uncommitted(SignedUnit<D>),
    NewForker(Box<Alert<D>>),
}

impl<D: Data> Debug for Error<D> {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        use Error::*;
        match self {
            Invalid(e) => write!(f, "Invalid({:?})", e),
            Duplicate(u) => write!(f, "Duplicate({:?})", u.clone().into_unchecked()),
            Uncommitted(u) => write!(f, "Uncommitted({:?})", u.clone().into_unchecked()),
            NewForker(a) => write!(f, "NewForker({:?})", a),
        }
    }
}

impl<D: Data> From<ValidationError<D>> for Error<D> {
    fn from(e: ValidationError<D>) -> Self {
        Error::Invalid(e)
    }
}

/// The summary status of the validator.
pub struct ValidatorStatus {
    processing_units: UnitStoreStatus,
    known_forkers: BTreeSet<PeerId>,
}

impl ValidatorStatus {
    /// The highest round among the units that are currently processing.
    pub fn top_round(&self) -> Round {
        self.processing_units.top_round()
    }
}

impl Display for ValidatorStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        write!(
            f,
            "processing units: ({}), forkers: {:?}",
            self.processing_units, self.known_forkers
        )
    }
}

type ValidatorResult<D> = Result<SignedUnit<D>, Error<D>>;

/// A validator that checks basic properties of units and catches forks.
pub struct Validator<D: Data> {
    unit_validator: UnitValidator,
    processing_units: UnitStore<SignedUnit<D>>,
    known_forkers: BTreeSet<PeerId>,
}

impl<D: Data> Validator<D> {
    /// A new validator using the provided unit validator under the hood.
    pub fn new(unit_validator: UnitValidator) -> Self {
        let node_count = unit_validator.node_count();
        Validator {
            unit_validator,
            processing_units: UnitStore::new(node_count),
            known_forkers: BTreeSet::new(),
        }
    }

    fn is_forker(&self, node_id: PeerId) -> bool {
        self.known_forkers.contains(&node_id)
    }

    fn mark_forker<U: WrappedUnit<Wrapped = SignedUnit<D>>>(
        &mut self,
        forker: PeerId,
        store: &UnitStore<U>,
    ) -> Vec<UncheckedSignedUnit<D>> {
        assert!(!self.is_forker(forker), "we shouldn't mark a forker twice");
        self.known_forkers.insert(forker);
        store
            .canonical_units(forker)
            .cloned()
            .map(WrappedUnit::unpack)
            // In principle we can have "canonical" processing units that are forks of store canonical units,
            // but only after we already marked a node as a forker, so not yet.
            // Also note that these units can be from different branches and we still commit to them here.
            // This is somewhat confusing, but not a problem for any theoretical guarantees.
            .chain(self.processing_units.canonical_units(forker).cloned())
            .map(|unit| unit.into_unchecked())
            .collect()
    }

    #[allow(clippy::result_large_err)]
    fn pre_validate<U: WrappedUnit<Wrapped = SignedUnit<D>>>(
        &mut self,
        unit: UncheckedSignedUnit<D>,
        store: &UnitStore<U>,
    ) -> ValidatorResult<D> {
        let unit = self.unit_validator.validate_unit(unit)?;
        let unit_hash = unit.as_signable().hash();
        if store.unit(&unit_hash).is_some() || self.processing_units.unit(&unit_hash).is_some() {
            return Err(Error::Duplicate(unit));
        }
        Ok(unit)
    }

    /// Validate an incoming unit.
    #[allow(clippy::result_large_err)]
    pub fn validate<U: WrappedUnit<Wrapped = SignedUnit<D>>>(
        &mut self,
        unit: UncheckedSignedUnit<D>,
        store: &UnitStore<U>,
    ) -> ValidatorResult<D> {
        use Error::*;
        let unit = self.pre_validate(unit, store)?;
        let unit_coord = unit.as_signable().coord();
        if self.is_forker(unit_coord.creator()) {
            return Err(Uncommitted(unit));
        }
        if let Some(canonical_unit) = store
            .canonical_unit(unit_coord)
            .map(|unit| unit.clone().unpack())
            .or(self.processing_units.canonical_unit(unit_coord).cloned())
        {
            let proof = (canonical_unit.into(), unit.into());
            let committed_units = self.mark_forker(unit_coord.creator(), store);
            return Err(NewForker(Box::new(Alert::new(
                self.unit_validator.index(),
                proof,
                committed_units,
            ))));
        }
        self.processing_units.insert(unit.clone());
        Ok(unit)
    }

    /// Validate a committed unit, it has to be from a forker.
    #[allow(clippy::result_large_err)]
    pub fn validate_committed<U: WrappedUnit<Wrapped = SignedUnit<D>>>(
        &mut self,
        unit: UncheckedSignedUnit<D>,
        store: &UnitStore<U>,
    ) -> ValidatorResult<D> {
        let unit = self.pre_validate(unit, store)?;
        assert!(
            self.is_forker(unit.creator()),
            "We should only receive committed units for known forkers."
        );
        self.processing_units.insert(unit.clone());
        Ok(unit)
    }

    /// The store of units currently being processed.
    pub fn processing_units(&self) -> &UnitStore<SignedUnit<D>> {
        &self.processing_units
    }

    /// Signal that a unit finished processing and thus it's copy no longer has to be kept for fork detection.
    /// NOTE: This is only a memory optimization, if the units stay there forever everything still works.
    pub fn finished_processing(&mut self, unit: &UnitHash) {
        self.processing_units.remove(unit)
    }

    /// The status summary of this validator.
    pub fn status(&self) -> ValidatorStatus {
        ValidatorStatus {
            processing_units: self.processing_units.status(),
            known_forkers: self.known_forkers.clone(),
        }
    }
}

#[cfg(test)]
mod test {
    use crate::{
        dag::validation::{Error, Validator},
        units::{
            random_full_parent_units_up_to, Unit, UnitStore, Validator as UnitValidator,
            WrappedSignedUnit,
        },
        NumPeers, PeerId, Signed,
    };
    use aleph_bft_mock::keychain;

    #[test]
    fn validates_trivially_correct() {
        let node_count = NumPeers::new(7_usize);
        let session_id = 0;
        let max_round = 2137;
        let keychains: Vec<_> = node_count
            .peer_ids()
            .map(|node_id| keychain(node_count, node_id))
            .collect();
        let store = UnitStore::<WrappedSignedUnit>::new(node_count);
        let mut validator = Validator::new(UnitValidator::new(
            session_id,
            keychains[0].clone(),
            max_round,
        ));
        for unit in random_full_parent_units_up_to(4, node_count, session_id)
            .iter()
            .flatten()
            .map(|unit| Signed::sign(unit.clone(), &keychains[unit.creator().to_usize()]))
        {
            assert_eq!(
                validator.validate(unit.clone().into(), &store),
                Ok(unit.clone())
            );
        }
    }

    #[test]
    fn refuses_processing_duplicates() {
        let node_count = NumPeers::new(7_usize);
        let session_id = 0;
        let max_round = 2137;
        let keychains: Vec<_> = node_count
            .peer_ids()
            .map(|node_id| keychain(node_count, node_id))
            .collect();
        let store = UnitStore::<WrappedSignedUnit>::new(node_count);
        let mut validator = Validator::new(UnitValidator::new(
            session_id,
            keychains[0].clone(),
            max_round,
        ));
        let unit = random_full_parent_units_up_to(0, node_count, session_id)
            .first()
            .expect("we have the first round")
            .first()
            .expect("we have the initial unit for the zeroth creator")
            .clone();
        let unit = Signed::sign(unit, &keychains[0]);
        assert_eq!(
            validator.validate(unit.clone().into(), &store),
            Ok(unit.clone())
        );
        assert_eq!(
            validator.validate(unit.clone().into(), &store),
            Err(Error::Duplicate(unit.clone()))
        );
    }

    #[test]
    fn refuses_external_duplicates() {
        let node_count = NumPeers::new(7_usize);
        let session_id = 0;
        let max_round = 2137;
        let keychains: Vec<_> = node_count
            .peer_ids()
            .map(|node_id| keychain(node_count, node_id))
            .collect();
        let mut store = UnitStore::new(node_count);
        let mut validator = Validator::new(UnitValidator::new(
            session_id,
            keychains[0].clone(),
            max_round,
        ));
        let unit = random_full_parent_units_up_to(0, node_count, session_id)
            .first()
            .expect("we have the first round")
            .first()
            .expect("we have the initial unit for the zeroth creator")
            .clone();
        let unit = Signed::sign(unit, &keychains[0]);
        store.insert(WrappedSignedUnit(unit.clone()));
        assert_eq!(
            validator.validate(unit.clone().into(), &store),
            Err(Error::Duplicate(unit.clone()))
        );
    }

    #[test]
    fn detects_processing_fork() {
        let node_count = NumPeers::new(7_usize);
        let session_id = 0;
        let max_round = 2137;
        let produced_round = 4;
        let keychains: Vec<_> = node_count
            .peer_ids()
            .map(|node_id| keychain(node_count, node_id))
            .collect();
        let store = UnitStore::<WrappedSignedUnit>::new(node_count);
        let mut validator = Validator::new(UnitValidator::new(
            session_id,
            keychains[0].clone(),
            max_round,
        ));
        for unit in random_full_parent_units_up_to(produced_round, node_count, session_id)
            .iter()
            .flatten()
            .map(|unit| Signed::sign(unit.clone(), &keychains[unit.creator().to_usize()]))
        {
            assert_eq!(
                validator.validate(unit.clone().into(), &store),
                Ok(unit.clone())
            );
        }
        let fork = random_full_parent_units_up_to(2, node_count, session_id)
            .get(2)
            .expect("we have the requested round")
            .first()
            .expect("we have the unit for the zeroth creator")
            .clone();
        let fork = Signed::sign(fork, &keychains[0]);
        assert!(matches!(
            validator.validate(fork.clone().into(), &store),
            Err(Error::NewForker(_))
        ));
    }

    #[test]
    fn detects_external_fork() {
        let node_count = NumPeers::new(7_usize);
        let session_id = 0;
        let max_round = 2137;
        let produced_round = 4;
        let keychains: Vec<_> = node_count
            .peer_ids()
            .map(|node_id| keychain(node_count, node_id))
            .collect();
        let mut store = UnitStore::new(node_count);
        let mut validator = Validator::new(UnitValidator::new(
            session_id,
            keychains[0].clone(),
            max_round,
        ));
        for unit in random_full_parent_units_up_to(produced_round, node_count, session_id)
            .iter()
            .flatten()
            .map(|unit| Signed::sign(unit.clone(), &keychains[unit.creator().to_usize()]))
        {
            store.insert(WrappedSignedUnit(unit));
        }
        let fork = random_full_parent_units_up_to(2, node_count, session_id)
            .get(2)
            .expect("we have the requested round")
            .first()
            .expect("we have the unit for the zeroth creator")
            .clone();
        let fork = Signed::sign(fork, &keychains[0]);
        assert!(matches!(
            validator.validate(fork.clone().into(), &store),
            Err(Error::NewForker(_))
        ));
    }

    #[test]
    fn refuses_uncommitted() {
        let node_count = NumPeers::new(7_usize);
        let session_id = 0;
        let max_round = 2137;
        let produced_round = 4;
        let keychains: Vec<_> = node_count
            .peer_ids()
            .map(|node_id| keychain(node_count, node_id))
            .collect();
        let store = UnitStore::<WrappedSignedUnit>::new(node_count);
        let mut validator = Validator::new(UnitValidator::new(
            session_id,
            keychains[0].clone(),
            max_round,
        ));
        let fork = random_full_parent_units_up_to(2, node_count, session_id)
            .get(2)
            .expect("we have the requested round")
            .first()
            .expect("we have the unit for the zeroth creator")
            .clone();
        let fork = Signed::sign(fork, &keychains[0]);
        for unit in random_full_parent_units_up_to(produced_round, node_count, session_id)
            .iter()
            .flatten()
            .filter(|unit| unit.creator() == PeerId::new(0_u8))
            .map(|unit| Signed::sign(unit.clone(), &keychains[unit.creator().to_usize()]))
        {
            match unit.round() {
                0..=1 => assert_eq!(
                    validator.validate(unit.clone().into(), &store),
                    Ok(unit.clone())
                ),
                2 => {
                    assert_eq!(
                        validator.validate(unit.clone().into(), &store),
                        Ok(unit.clone())
                    );
                    assert!(matches!(
                        validator.validate(fork.clone().into(), &store),
                        Err(Error::NewForker(_))
                    ))
                }
                3.. => assert_eq!(
                    validator.validate(unit.clone().into(), &store),
                    Err(Error::Uncommitted(unit.clone()))
                ),
            }
        }
    }

    #[test]
    fn accepts_committed() {
        let node_count = NumPeers::new(7_usize);
        let session_id = 0;
        let max_round = 2137;
        let produced_round = 4;
        let keychains: Vec<_> = node_count
            .peer_ids()
            .map(|node_id| keychain(node_count, node_id))
            .collect();
        let store = UnitStore::<WrappedSignedUnit>::new(node_count);
        let mut validator = Validator::new(UnitValidator::new(
            session_id,
            keychains[0].clone(),
            max_round,
        ));
        let fork = random_full_parent_units_up_to(2, node_count, session_id)
            .get(2)
            .expect("we have the requested round")
            .first()
            .expect("we have the unit for the zeroth creator")
            .clone();
        let fork = Signed::sign(fork, &keychains[0]);
        let units: Vec<_> = random_full_parent_units_up_to(produced_round, node_count, session_id)
            .iter()
            .flatten()
            .filter(|unit| unit.creator() == PeerId::new(0_u8))
            .map(|unit| Signed::sign(unit.clone(), &keychains[unit.creator().to_usize()]))
            .collect();
        for unit in units.iter().take(3) {
            assert_eq!(
                validator.validate(unit.clone().into(), &store),
                Ok(unit.clone())
            );
        }
        assert!(matches!(
            validator.validate(fork.clone().into(), &store),
            Err(Error::NewForker(_))
        ));
        assert_eq!(
            validator.validate_committed(fork.clone().into(), &store),
            Ok(fork)
        );
    }
}
