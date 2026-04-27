//! Test-only in-memory implementations of [`BackupSink`] and [`BackupSource`].

use std::{io, sync::Arc};

use parking_lot::Mutex;

use crate::{
    backup::{BackupSink, BackupSource},
    units::UncheckedSignedUnit,
    Data,
};

/// Append-only in-memory sink. Saved units land in a shared `Vec` so tests
/// can either inspect them directly or convert them into a [`MockSource`].
#[derive(Clone, Debug, Default)]
pub struct MockSink<D: Data> {
    data: Arc<Mutex<Vec<UncheckedSignedUnit<D>>>>,
}

impl<D: Data> MockSink<D> {
    pub fn new() -> Self {
        Self {
            data: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Backed by the same `Arc<Mutex<...>>` as `state`, so saves are visible
    /// to anyone holding `state`. Used by tests that need to inspect what
    /// the consensus layer wrote.
    pub fn from_shared(data: Arc<Mutex<Vec<UncheckedSignedUnit<D>>>>) -> Self {
        Self { data }
    }
}

impl<D: Data> BackupSink<D> for MockSink<D> {
    fn save(&mut self, unit: UncheckedSignedUnit<D>) -> io::Result<()> {
        self.data.lock().push(unit);
        Ok(())
    }
}

/// In-memory source seeded with a fixed `Vec` of units, returned verbatim
/// (in order) on [`load`].
#[derive(Clone, Debug, Default)]
pub struct MockSource<D: Data> {
    data: Vec<UncheckedSignedUnit<D>>,
}

impl<D: Data> MockSource<D> {
    pub fn new(data: Vec<UncheckedSignedUnit<D>>) -> Self {
        Self { data }
    }
}

impl<D: Data> BackupSource<D> for MockSource<D> {
    fn load(self) -> io::Result<Vec<UncheckedSignedUnit<D>>> {
        Ok(self.data)
    }
}
