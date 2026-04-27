use std::{
    fmt::{self, Debug},
    marker::PhantomData,
};

use crate::{
    backup::BackupSource,
    units::{UncheckedSignedUnit, Unit, UnitCoord},
    Data, PeerId, Round, SessionId,
};

/// Backup load error.
#[derive(Debug)]
pub enum LoaderError {
    IO(std::io::Error),
    InconsistentData(UnitCoord),
    WrongSession(UnitCoord, SessionId, SessionId),
}

impl fmt::Display for LoaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoaderError::IO(err) => {
                write!(
                    f,
                    "received IO error while reading from backup source: {}",
                    err
                )
            }
            LoaderError::InconsistentData(coord) => {
                write!(
                    f,
                    "inconsistent backup data. Unit from round {:?} of creator {:?} is missing a parent in backup.",
                    coord.round(), coord.creator()
                )
            }
            LoaderError::WrongSession(coord, expected_session, actual_session) => {
                write!(
                    f,
                    "unit from round {:?} of creator {:?} has a wrong session id in backup. Expected: {:?} got: {:?}",
                    coord.round(), coord.creator(), expected_session, actual_session
                )
            }
        }
    }
}

impl From<std::io::Error> for LoaderError {
    fn from(err: std::io::Error) -> Self {
        Self::IO(err)
    }
}

pub struct BackupLoader<D: Data, S: BackupSource<D>> {
    backup: S,
    index: PeerId,
    session_id: SessionId,
    _phantom: PhantomData<D>,
}

impl<D: Data, S: BackupSource<D>> BackupLoader<D, S> {
    pub fn new(backup: S, index: PeerId, session_id: SessionId) -> Self {
        BackupLoader {
            backup,
            index,
            session_id,
            _phantom: PhantomData,
        }
    }

    pub async fn load_backup(self) -> Result<(Vec<UncheckedSignedUnit<D>>, Round), LoaderError> {
        let Self {
            backup,
            index,
            session_id,
            ..
        } = self;
        let units = backup.load()?;
        verify_units(&units, session_id)?;
        let next_round: Round = units
            .iter()
            .filter(|u| u.as_signable().creator() == index)
            .map(|u| u.as_signable().round())
            .max()
            .map(|round| round + 1)
            .unwrap_or(0);

        Ok((units, next_round))
    }
}

fn verify_units<D: Data>(
    units: &Vec<UncheckedSignedUnit<D>>,
    session_id: SessionId,
) -> Result<(), LoaderError> {
    let mut already_loaded_coords = std::collections::HashSet::new();

    for unit in units {
        let full_unit = unit.as_signable();
        let coord = full_unit.coord();

        if full_unit.session_id() != session_id {
            return Err(LoaderError::WrongSession(
                coord,
                session_id,
                full_unit.session_id(),
            ));
        }

        for parent in full_unit.as_pre_unit().control_hash().parents() {
            if !already_loaded_coords.contains(&parent) {
                return Err(LoaderError::InconsistentData(coord));
            }
        }

        already_loaded_coords.insert(coord);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use aleph_bft_mock::{keychain, Data};

    use crate::{
        backup::{loader::LoaderError, mock::MockSource, BackupLoader as GenericLoader},
        units::{
            create_preunits, creator_set, preunit_to_full_unit, preunit_to_unchecked_signed_unit,
            UncheckedSignedUnit as GenericUncheckedSignedUnit,
        },
        NumPeers, PeerId, Round, SessionId,
    };

    type UncheckedSignedUnit = GenericUncheckedSignedUnit<Data>;
    type BackupLoader = GenericLoader<Data, MockSource<Data>>;

    const SESSION_ID: SessionId = 43;
    const NODE_ID: PeerId = PeerId::new(0 as u8);
    const N_MEMBERS: NumPeers = NumPeers::new(4 as usize);

    fn produce_units(rounds: usize, session_id: SessionId) -> Vec<Vec<UncheckedSignedUnit>> {
        let mut creators = creator_set(N_MEMBERS);
        let keychains: Vec<_> = (0..N_MEMBERS.total())
            .map(|id| keychain(N_MEMBERS, PeerId::new(id as u8)))
            .collect();

        let mut units_per_round = Vec::with_capacity(rounds);

        for round in 0..rounds {
            let pre_units = create_preunits(creators.iter(), round as Round);

            let units: Vec<_> = pre_units
                .iter()
                .map(|pre_unit| preunit_to_full_unit(pre_unit.clone(), session_id))
                .collect();
            for creator in creators.iter_mut() {
                creator.add_units(&units);
            }

            let mut unchecked_signed_units = Vec::with_capacity(pre_units.len());
            for (pre_unit, kc) in pre_units.into_iter().zip(keychains.iter()) {
                unchecked_signed_units
                    .push(preunit_to_unchecked_signed_unit(pre_unit, session_id, kc))
            }

            units_per_round.push(unchecked_signed_units);
        }

        units_per_round
    }

    fn units_of_creator(
        units: Vec<Vec<UncheckedSignedUnit>>,
        creator: PeerId,
    ) -> Vec<UncheckedSignedUnit> {
        units
            .into_iter()
            .map(|units_per_round| units_per_round[creator.to_usize()].clone())
            .collect()
    }

    #[tokio::test]
    async fn loads_nothing() {
        let (units, round) = BackupLoader::new(MockSource::new(Vec::new()), NODE_ID, SESSION_ID)
            .load_backup()
            .await
            .expect("should load correctly");
        assert_eq!(round, 0);
        assert_eq!(units, Vec::new());
    }

    #[tokio::test]
    async fn loads_some_units() {
        let items: Vec<_> = produce_units(5, SESSION_ID).into_iter().flatten().collect();

        let (units, round) = BackupLoader::new(MockSource::new(items.clone()), NODE_ID, SESSION_ID)
            .load_backup()
            .await
            .expect("should load correctly");
        assert_eq!(round, 5);
        assert_eq!(units, items);
    }

    #[tokio::test]
    async fn backup_with_missing_parent_fails() {
        let mut items: Vec<_> = produce_units(5, SESSION_ID).into_iter().flatten().collect();
        items.remove(2);

        assert!(matches!(
            BackupLoader::new(MockSource::new(items), NODE_ID, SESSION_ID)
                .load_backup()
                .await,
            Err(LoaderError::InconsistentData(_))
        ));
    }

    #[tokio::test]
    async fn backup_with_units_of_one_creator_fails() {
        let items = units_of_creator(
            produce_units(5, SESSION_ID),
            PeerId::from((NODE_ID.to_usize() + 1) as u8),
        );

        assert!(matches!(
            BackupLoader::new(MockSource::new(items), NODE_ID, SESSION_ID)
                .load_backup()
                .await,
            Err(LoaderError::InconsistentData(_))
        ));
    }

    #[tokio::test]
    async fn backup_with_wrong_session_fails() {
        let items: Vec<_> = produce_units(5, SESSION_ID + 1)
            .into_iter()
            .flatten()
            .collect();

        assert!(matches!(
            BackupLoader::new(MockSource::new(items), NODE_ID, SESSION_ID)
                .load_backup()
                .await,
            Err(LoaderError::WrongSession(..))
        ));
    }
}
