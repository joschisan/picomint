use std::sync::Arc;

use crate::graph::Entry;
use crate::unit::UnitData;

/// Crash-recovery store for one peer's [`crate::Graph`] state.
///
/// The engine calls [`Backup::save`] after every state-mutating step —
/// every `insert_unit → Accepted` and every `record_sig` that recorded a
/// signature — passing the post-mutation [`Entry`]. Backends key by the
/// entry's `(round, creator)` slot and overwrite in place, so a slot
/// occupies one row regardless of how many sigs accumulate.
///
/// On startup the engine calls [`Backup::load`] before driving any
/// timers and re-populates its `Graph` via `Graph::restore_entry`.
/// Returning entries in canonical `(round, peer)` lex order means
/// parents always restore before children, so no separate sequencing is
/// required.
pub trait Backup<D: UnitData>: Send + Sync + 'static {
    /// Durably persist the current state of the entry's slot. Must be
    /// flushed on return — the engine treats this as a crash barrier
    /// before broadcasting the unit it just inserted.
    fn save(&self, entry: &Entry<D>);

    /// Load all persisted entries in `(round, peer)` lex order.
    fn load(&self) -> Vec<Entry<D>>;
}

/// `Arc`-erased [`Backup`]. The engine consumes this so callers can swap
/// in any concrete implementation — a no-op for tests, a redb-backed
/// store for the daemon.
pub type DynBackup<D> = Arc<dyn Backup<D>>;

/// Drops every save and loads nothing. Default for tests and any
/// deployment that doesn't need crash recovery.
pub struct NoopBackup;

impl<D: UnitData> Backup<D> for NoopBackup {
    fn save(&self, _entry: &Entry<D>) {}

    fn load(&self) -> Vec<Entry<D>> {
        Vec::new()
    }
}
