use std::io;

use aleph_bft::{BackupSink, BackupSource, UncheckedSignedUnit};
use picomint_core::transaction::ConsensusItem;
use picomint_redb::Database;
use tracing::info;

use crate::LOG_CONSENSUS;
use crate::consensus::db::{ALEPH_UNITS, AlephUnit};

pub struct UnitLoader {
    db: Database,
}

impl UnitLoader {
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

impl BackupSource<Vec<ConsensusItem>> for UnitLoader {
    fn load(self) -> io::Result<Vec<UncheckedSignedUnit<Vec<ConsensusItem>>>> {
        let units: Vec<UncheckedSignedUnit<Vec<ConsensusItem>>> = self
            .db
            .begin_read()
            .iter(&ALEPH_UNITS, |r| r.map(|(_, AlephUnit(u))| u).collect());

        if !units.is_empty() {
            info!(
                target: LOG_CONSENSUS,
                units_len = %units.len(),
                "Recovering from an in-session-shutdown"
            );
        }

        Ok(units)
    }
}

pub struct UnitSaver {
    db: Database,
    units_index: u64,
}

impl UnitSaver {
    pub fn new(db: Database) -> Self {
        let units_index = db
            .begin_read()
            .iter(&ALEPH_UNITS, |r| r.next_back().map_or(0, |(k, _)| k + 1));

        Self { db, units_index }
    }
}

impl BackupSink<Vec<ConsensusItem>> for UnitSaver {
    fn save(&mut self, unit: UncheckedSignedUnit<Vec<ConsensusItem>>) -> io::Result<()> {
        let tx = self.db.begin_write();
        tx.insert(&ALEPH_UNITS, &self.units_index, &AlephUnit(unit));
        tx.commit();
        self.units_index += 1;
        Ok(())
    }
}
