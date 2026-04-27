use std::io;

use crate::{units::UncheckedSignedUnit, Data};

mod loader;
#[cfg(test)]
pub(crate) mod mock;
mod saver;

pub use loader::BackupLoader;
pub use saver::BackupSaver;

/// Persistence sink for in-progress aleph-bft units.
///
/// `save` is called once per unit before it is added to the dag — implementors
/// must persist the unit durably (and in call order) before returning.
pub trait BackupSink<D: Data>: Send + 'static {
    fn save(&mut self, unit: UncheckedSignedUnit<D>) -> io::Result<()>;
}

/// Persistence source for in-progress aleph-bft units.
///
/// `load` is called once at session start and must return units in the same
/// order they were originally passed to [`BackupSink::save`].
pub trait BackupSource<D: Data>: Send + 'static {
    fn load(self) -> io::Result<Vec<UncheckedSignedUnit<D>>>;
}
