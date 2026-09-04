//! redb-backed database for picomint.
//!
//! Tables are typed handles implementing the [`Table`] trait, declared with
//! the [`table!`] macro. Every table maps to one redb table of raw byte
//! strings (`&[u8] => &[u8]`): keys and values are serialized via consensus
//! encoding, with tuple keys encoding as the plain concatenation of their
//! elements. redb's typed `Key`/`Value` machinery is touched only through
//! the single byte-table definition — no per-type trait impls exist.
//!
//! No redb type appears in this crate's public API — callers interact
//! exclusively through typed tables, keys, values and prefixes.
//!
//! Ordering: redb compares byte keys lexicographically, and consensus
//! encoding is fixed-width big-endian for integers and raw bytes for
//! hashes, so byte order matches the elements' logical order and — because
//! every key element's encoding is prefix-free — concatenation preserves
//! tuple order. Iteration, prefix and range order therefore equal
//! element-wise tuple order. A variable-length, non-prefix-free key element
//! (e.g. a length-prefixed `String`) would break this contract.
//!
//! Concurrency is redb's native model: a single write transaction at a time
//! (redb serializes writers internally) and snapshot-isolated readers
//! (MVCC, a snapshot per [`Database::begin_read`]).

use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;
use std::ops::{Bound, RangeBounds};
use std::path::Path;
use std::sync::{Arc, Mutex};

use picomint_encoding::{Decodable, Encodable};
use redb::{Durability, Range, ReadableDatabase, TableDefinition};
use tokio::sync::Notify;

/// Decode a key or value from its stored bytes.
fn decode<T: Decodable>(bytes: &[u8]) -> T {
    T::consensus_decode(bytes).expect("stored bytes failed to decode")
}

/// All tables store raw consensus-encoded bytes; redb's typed layer is
/// entered only through this single `&[u8] => &[u8]` definition.
fn byte_table(name: &str) -> TableDefinition<'_, &'static [u8], &'static [u8]> {
    TableDefinition::new(name)
}

// ─── Table trait + Prefix + table-declaring macro ────────────────────────

/// A typed table reference. Implementors pair key/value types with a fixed
/// on-disk table name. Implemented by the [`table!`] macro.
pub trait Table {
    type Key: Encodable + Decodable;
    type Value: Encodable + Decodable;

    /// On-disk table name.
    fn name(&self) -> &'static str;
}

/// A typed proper prefix of `D`'s key tuple: its consensus encoding is a
/// byte prefix of the full key's encoding, because tuples encode as the
/// plain concatenation of their elements. Implemented by the [`table!`]
/// macro for the bare first element and every longer proper prefix tuple,
/// e.g. a `(A, B, C)` key yields impls for `A` and `(A, B)`.
pub trait Prefix<D: Table>: Encodable {}

/// Declare a table. Expands to a zero-sized unit struct implementing
/// [`Table`], plus [`Prefix`] impls for every proper prefix of a tuple key.
///
/// ```ignore
/// table!(
///     NoteTable,
///     (MintId, Account, SpendableNote) => (),
///     "note",
/// );
/// ```
#[macro_export]
macro_rules! table {
    (
        $(#[$attr:meta])*
        $name:ident,
        ($k0:ty, $k1:ty, $k2:ty) => $v:ty,
        $label:literal $(,)?
    ) => {
        $(#[$attr])*
        #[derive(Copy, Clone, Debug)]
        pub struct $name;

        impl $crate::Table for $name {
            type Key = ($k0, $k1, $k2);
            type Value = $v;

            fn name(&self) -> &'static str {
                $label
            }
        }

        impl $crate::Prefix<$name> for $k0 {}

        impl $crate::Prefix<$name> for ($k0, $k1) {}
    };
    (
        $(#[$attr:meta])*
        $name:ident,
        ($k0:ty, $k1:ty) => $v:ty,
        $label:literal $(,)?
    ) => {
        $(#[$attr])*
        #[derive(Copy, Clone, Debug)]
        pub struct $name;

        impl $crate::Table for $name {
            type Key = ($k0, $k1);
            type Value = $v;

            fn name(&self) -> &'static str {
                $label
            }
        }

        impl $crate::Prefix<$name> for $k0 {}
    };
    (
        $(#[$attr:meta])*
        $name:ident,
        $k:ty => $v:ty,
        $label:literal $(,)?
    ) => {
        $(#[$attr])*
        #[derive(Copy, Clone, Debug)]
        pub struct $name;

        impl $crate::Table for $name {
            type Key = $k;
            type Value = $v;

            fn name(&self) -> &'static str {
                $label
            }
        }
    };
}

// ─── Key ranges ──────────────────────────────────────────────────────────

/// Encoded byte bounds for a typed key range.
fn encoded_bounds<K: Encodable>(range: &impl RangeBounds<K>) -> (Bound<Vec<u8>>, Bound<Vec<u8>>) {
    let encode_bound = |bound: Bound<&K>| match bound {
        Bound::Included(key) => Bound::Included(key.consensus_encode_to_vec()),
        Bound::Excluded(key) => Bound::Excluded(key.consensus_encode_to_vec()),
        Bound::Unbounded => Bound::Unbounded,
    };

    (
        encode_bound(range.start_bound()),
        encode_bound(range.end_bound()),
    )
}

/// Byte bounds covering exactly the keys that start with `prefix` — the
/// keys with prefix bytes form one contiguous run in lexicographic order.
fn prefix_bounds(prefix: Vec<u8>) -> (Bound<Vec<u8>>, Bound<Vec<u8>>) {
    let upper = match prefix_upper_bound(&prefix) {
        Some(bound) => Bound::Excluded(bound),
        None => Bound::Unbounded,
    };

    (Bound::Included(prefix), upper)
}

/// The smallest byte string strictly greater than every string starting
/// with `prefix`: the prefix with its last non-0xFF byte incremented and
/// the tail dropped. `None` if the prefix is empty or all 0xFF — no such
/// string exists and the range is unbounded above.
fn prefix_upper_bound(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut bound = prefix.to_vec();

    while let Some(last) = bound.last_mut() {
        if *last < u8::MAX {
            *last += 1;

            return Some(bound);
        }

        bound.pop();
    }

    None
}

fn as_slices(bounds: &(Bound<Vec<u8>>, Bound<Vec<u8>>)) -> (Bound<&[u8]>, Bound<&[u8]>) {
    (
        bounds.0.as_ref().map(|bound| bound.as_slice()),
        bounds.1.as_ref().map(|bound| bound.as_slice()),
    )
}

const UNBOUNDED: (Bound<Vec<u8>>, Bound<Vec<u8>>) = (Bound::Unbounded, Bound::Unbounded);

// ─── Database ────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Database {
    inner: Arc<DatabaseInner>,
}

struct DatabaseInner {
    env: redb::Database,
    /// Lazily-populated map of table name -> shared `Notify`. Any commit
    /// that wrote a table wakes every waiter on that table.
    notify: Mutex<BTreeMap<&'static str, Arc<Notify>>>,
}

impl DatabaseInner {
    fn notify_for(&self, name: &'static str) -> Arc<Notify> {
        self.notify
            .lock()
            .expect("notify map poisoned")
            .entry(name)
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone()
    }
}

impl Database {
    fn new(env: redb::Database) -> Self {
        Self {
            inner: Arc::new(DatabaseInner {
                env,
                notify: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    /// Open (or create) a redb database at `path`. The only fallible entry
    /// point; every other public method panics internally on redb errors.
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        Ok(Self::new(redb::Database::create(path.as_ref())?))
    }

    /// Open a fresh, private in-memory database. Intended for tests and
    /// ephemeral dev use.
    pub fn open_in_memory() -> Self {
        let env = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .expect("in-memory redb create failed");

        Self::new(env)
    }

    pub fn begin_write(&self) -> WriteTx {
        WriteTx {
            tx: self
                .inner
                .env
                .begin_write()
                .expect("redb begin_write failed"),
            db: self.inner.clone(),
            touched: Mutex::new(BTreeSet::new()),
            on_commit: Mutex::new(Vec::new()),
        }
    }

    /// Begin a write transaction whose commit does **not** fsync. The writes
    /// become durable only when a later [`Self::begin_write`] commit flushes
    /// them, and a crash rolls back to that last durable commit. Use only
    /// for state a node can reconstruct after a crash (e.g. BFT units
    /// re-fetched from peers) — never for money-bearing or safety-critical
    /// writes.
    pub fn begin_write_relaxed(&self) -> WriteTx {
        let mut tx = self
            .inner
            .env
            .begin_write()
            .expect("redb begin_write failed");

        tx.set_durability(Durability::None)
            .expect("set_durability only fails with a persistent savepoint, which we never create");

        WriteTx {
            tx,
            db: self.inner.clone(),
            touched: Mutex::new(BTreeSet::new()),
            on_commit: Mutex::new(Vec::new()),
        }
    }

    pub fn begin_read(&self) -> ReadTx {
        ReadTx {
            tx: self.inner.env.begin_read().expect("redb begin_read failed"),
        }
    }

    /// Shared [`Notify`] handle for `table`. Fires via `notify_waiters` on
    /// every commit that wrote the table.
    ///
    /// Callers must construct `notified()` *before* the check it guards —
    /// tokio's `Notified` captures the `notify_waiters` generation at
    /// construction time, so any commit that lands between the check and the
    /// await will still wake the waiter.
    pub fn notify_for_table<T: Table>(&self, def: &T) -> Arc<Notify> {
        self.inner.notify_for(def.name())
    }

    /// Wait until `check` returns `Some(T)`, then return `(T, ReadTx)`.
    /// The returned tx is the one that observed the matched state. `check` is
    /// called once on entry and again after every commit that touches `table`.
    pub async fn wait_table_check<D, R>(
        &self,
        def: &D,
        mut check: impl FnMut(&ReadTx) -> Option<R>,
    ) -> (R, ReadTx)
    where
        D: Table,
    {
        let notify = self.notify_for_table(def);

        loop {
            let notified = notify.notified();

            let dbtx = self.begin_read();

            if let Some(t) = check(&dbtx) {
                return (t, dbtx);
            }

            drop(dbtx);

            notified.await;
        }
    }
}

// ─── Transactions ────────────────────────────────────────────────────────

pub struct WriteTx {
    tx: redb::WriteTransaction,
    db: Arc<DatabaseInner>,
    /// Names of tables written during this tx, used to notify waiters on
    /// commit.
    touched: Mutex<BTreeSet<&'static str>>,
    on_commit: Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>,
}

pub struct ReadTx {
    tx: redb::ReadTransaction,
}

impl WriteTx {
    /// Register a callback to run after a successful commit.
    pub fn on_commit(&self, f: impl FnOnce() + Send + 'static) {
        self.on_commit
            .lock()
            .expect("on_commit poisoned")
            .push(Box::new(f));
    }

    /// Commit the transaction. Dropping a [`WriteTx`] without calling this
    /// rolls it back (redb aborts on drop) and its callbacks never run.
    pub fn commit(self) {
        let Self {
            tx,
            db,
            touched,
            on_commit,
        } = self;

        tx.commit().expect("redb commit failed");

        for name in touched.into_inner().expect("touched poisoned") {
            db.notify_for(name).notify_waiters();
        }

        for cb in on_commit.into_inner().expect("on_commit poisoned") {
            cb();
        }
    }

    fn open(&self, name: &str) -> redb::Table<'_, &'static [u8], &'static [u8]> {
        self.tx
            .open_table(byte_table(name))
            .expect("redb open_table failed")
    }

    fn touch<D: Table>(&self, def: &D) {
        self.touched
            .lock()
            .expect("touched poisoned")
            .insert(def.name());
    }
}

// ─── Typed ops ───────────────────────────────────────────────────────────

/// Streaming iterator over a table scan. Wraps the underlying redb range so
/// callers receive owned, decoded `(K, V)` pairs directly. A table that has
/// never been created iterates as empty (`rows` is `None`).
pub struct TableIter<'a, D: Table> {
    rows: Option<Range<'a, &'static [u8], &'static [u8]>>,
    rev: bool,
    _table: PhantomData<D>,
}

impl<D: Table> Iterator for TableIter<'_, D> {
    type Item = (D::Key, D::Value);

    fn next(&mut self) -> Option<Self::Item> {
        let rows = self.rows.as_mut()?;

        let row = match self.rev {
            true => rows.next_back(),
            false => rows.next(),
        };

        let (key, value) = row?.expect("redb row read failed");

        Some((decode(key.value()), decode(value.value())))
    }
}

mod sealed {
    use std::ops::Bound;

    use redb::{AccessGuard, Range, ReadableTable as _};

    use super::byte_table;

    /// One table of a tx, opened for the duration of one op. Unifies the
    /// write and read table handles behind the two shared accessors the
    /// provided read methods need.
    pub enum TableHandle<'a> {
        Write(redb::Table<'a, &'static [u8], &'static [u8]>),
        Read(redb::ReadOnlyTable<&'static [u8], &'static [u8]>),
    }

    impl TableHandle<'_> {
        pub(super) fn get(&self, key: &[u8]) -> Option<AccessGuard<'_, &'static [u8]>> {
            match self {
                TableHandle::Write(table) => table.get(key),
                TableHandle::Read(table) => table.get(key),
            }
            .expect("redb get failed")
        }

        pub(super) fn range(
            &self,
            bounds: (Bound<&[u8]>, Bound<&[u8]>),
        ) -> Range<'_, &'static [u8], &'static [u8]> {
            match self {
                TableHandle::Write(table) => table.range::<&[u8]>(bounds),
                TableHandle::Read(table) => table.range::<&[u8]>(bounds),
            }
            .expect("redb range failed")
        }
    }

    /// Sealed supertrait of [`DbRead`](super::DbRead): restricts the trait
    /// to the two tx types and hands their tables to the provided read
    /// methods. Unnameable outside the crate.
    pub trait HasTables {
        /// Run `f` over the table at `name`; `None` if it has never been
        /// created (a write tx creates it instead, mirroring redb).
        fn with_table<R>(
            &self,
            name: &'static str,
            f: impl FnOnce(Option<TableHandle<'_>>) -> R,
        ) -> R;
    }

    impl HasTables for super::WriteTx {
        fn with_table<R>(
            &self,
            name: &'static str,
            f: impl FnOnce(Option<TableHandle<'_>>) -> R,
        ) -> R {
            f(Some(TableHandle::Write(self.open(name))))
        }
    }

    impl HasTables for super::ReadTx {
        fn with_table<R>(
            &self,
            name: &'static str,
            f: impl FnOnce(Option<TableHandle<'_>>) -> R,
        ) -> R {
            match self.tx.open_table(byte_table(name)) {
                Ok(table) => f(Some(TableHandle::Read(table))),
                Err(redb::TableError::TableDoesNotExist(_)) => f(None),
                Err(e) => panic!("redb open_table failed: {e}"),
            }
        }
    }
}

use sealed::HasTables;

/// Run `f` over the decoded rows within `bounds`. A backing table that has
/// never been created scans as empty — `f` always runs, so closure results
/// on an empty iterator (e.g. `all` returning `true`) hold either way.
fn scan<T, D, R>(
    tx: &T,
    def: &D,
    bounds: (Bound<Vec<u8>>, Bound<Vec<u8>>),
    rev: bool,
    f: impl FnOnce(&mut TableIter<'_, D>) -> R,
) -> R
where
    T: HasTables + ?Sized,
    D: Table,
{
    tx.with_table(def.name(), |table| match table {
        Some(handle) => f(&mut TableIter {
            rows: Some(handle.range(as_slices(&bounds))),
            rev,
            _table: PhantomData,
        }),
        None => f(&mut TableIter {
            rows: None,
            rev,
            _table: PhantomData,
        }),
    })
}

/// Typed read operations, shared by both tx types. A table that has never
/// been written reads as empty on a [`ReadTx`] — `None` for gets, an empty
/// iteration for scans.
pub trait DbRead: HasTables {
    fn get<D: Table>(&self, def: &D, key: &D::Key) -> Option<D::Value> {
        self.with_table(def.name(), |table| {
            table?
                .get(&key.consensus_encode_to_vec())
                .map(|value| decode(value.value()))
        })
    }

    fn iter<D: Table, R>(&self, def: &D, f: impl FnOnce(&mut TableIter<'_, D>) -> R) -> R {
        scan(self, def, UNBOUNDED, false, f)
    }

    /// Iterate the table in descending key order.
    fn iter_rev<D: Table, R>(&self, def: &D, f: impl FnOnce(&mut TableIter<'_, D>) -> R) -> R {
        scan(self, def, UNBOUNDED, true, f)
    }

    /// Iterate every entry whose key starts with `prefix`, in ascending key
    /// order. Compiles to one contiguous byte-range scan.
    fn prefix<D: Table, P: Prefix<D>, R>(
        &self,
        def: &D,
        prefix: &P,
        f: impl FnOnce(&mut TableIter<'_, D>) -> R,
    ) -> R {
        let bounds = prefix_bounds(prefix.consensus_encode_to_vec());

        scan(self, def, bounds, false, f)
    }

    /// Iterate every entry whose key starts with `prefix`, in descending key
    /// order.
    fn prefix_rev<D: Table, P: Prefix<D>, R>(
        &self,
        def: &D,
        prefix: &P,
        f: impl FnOnce(&mut TableIter<'_, D>) -> R,
    ) -> R {
        let bounds = prefix_bounds(prefix.consensus_encode_to_vec());

        scan(self, def, bounds, true, f)
    }

    /// Iterate the entries within a typed key range, in ascending key order.
    fn range<D: Table, B: RangeBounds<D::Key>, R>(
        &self,
        def: &D,
        range: B,
        f: impl FnOnce(&mut TableIter<'_, D>) -> R,
    ) -> R {
        let bounds = encoded_bounds(&range);

        scan(self, def, bounds, false, f)
    }
}

impl DbRead for ReadTx {}
impl DbRead for WriteTx {}

impl WriteTx {
    /// Insert `value` under `key`, returning the previously stored value.
    pub fn insert<D: Table>(&self, def: &D, key: &D::Key, value: &D::Value) -> Option<D::Value> {
        self.touch(def);

        self.open(def.name())
            .insert(
                key.consensus_encode_to_vec().as_slice(),
                value.consensus_encode_to_vec().as_slice(),
            )
            .expect("redb insert failed")
            .map(|previous| decode(previous.value()))
    }

    /// Remove the entry under `key`, returning the previously stored value.
    pub fn remove<D: Table>(&self, def: &D, key: &D::Key) -> Option<D::Value> {
        self.touch(def);

        self.open(def.name())
            .remove(key.consensus_encode_to_vec().as_slice())
            .expect("redb remove failed")
            .map(|previous| decode(previous.value()))
    }

    /// Remove every entry whose key starts with `prefix`. Used to clear one
    /// scope's slice of a shared table, e.g. a mint's rows on leave.
    pub fn remove_prefix<D: Table, P: Prefix<D>>(&self, def: &D, prefix: &P) {
        self.touch(def);

        let bounds = prefix_bounds(prefix.consensus_encode_to_vec());

        self.open(def.name())
            .retain_in::<&[u8], fn(&[u8], &[u8]) -> bool>(as_slices(&bounds), |_, _| false)
            .expect("redb retain_in failed");
    }

    /// Remove every entry of the table. Deletes the redb table outright —
    /// safe here, unlike SQLite, because MVCC readers keep iterating their
    /// own snapshot; the next write recreates it.
    pub fn clear_table<D: Table>(&self, def: &D) {
        self.touch(def);

        self.tx
            .delete_table(byte_table(def.name()))
            .expect("redb delete_table failed");
    }
}

pub trait DbWrite: DbRead {
    fn insert<D: Table>(&self, def: &D, key: &D::Key, value: &D::Value) -> Option<D::Value>;

    fn remove<D: Table>(&self, def: &D, key: &D::Key) -> Option<D::Value>;

    fn remove_prefix<D: Table, P: Prefix<D>>(&self, def: &D, prefix: &P);

    fn clear_table<D: Table>(&self, def: &D);
}

impl DbWrite for WriteTx {
    fn insert<D: Table>(&self, def: &D, key: &D::Key, value: &D::Value) -> Option<D::Value> {
        WriteTx::insert(self, def, key, value)
    }

    fn remove<D: Table>(&self, def: &D, key: &D::Key) -> Option<D::Value> {
        WriteTx::remove(self, def, key)
    }

    fn remove_prefix<D: Table, P: Prefix<D>>(&self, def: &D, prefix: &P) {
        WriteTx::remove_prefix(self, def, prefix)
    }

    fn clear_table<D: Table>(&self, def: &D) {
        WriteTx::clear_table(self, def)
    }
}

// ─── Playground tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::*;

    table!(UsersTable, u64 => String, "users");
    table!(BalancesTable, u64 => u64, "balances");
    table!(NotesTable, (u64, u32, String) => (), "notes");
    table!(AddressTable, (u64, u32, u64) => (), "addresses");

    #[test]
    fn basic_read_write() {
        let db = Database::open_in_memory();

        let tx = db.begin_write();
        tx.insert(&UsersTable, &1, &"alice".to_string());
        tx.insert(&UsersTable, &2, &"bob".to_string());
        tx.insert(&BalancesTable, &1, &100);
        tx.commit();

        let tx = db.begin_read();
        assert_eq!(tx.get(&UsersTable, &1), Some("alice".to_string()));
        assert_eq!(tx.get(&UsersTable, &2), Some("bob".to_string()));
        assert_eq!(tx.get(&UsersTable, &3), None);
        assert_eq!(tx.get(&BalancesTable, &1), Some(100));
    }

    #[test]
    fn insert_and_remove_return_previous_value() {
        let db = Database::open_in_memory();

        let tx = db.begin_write();
        assert_eq!(tx.insert(&BalancesTable, &1, &100), None);
        assert_eq!(tx.insert(&BalancesTable, &1, &200), Some(100));
        assert_eq!(tx.remove(&BalancesTable, &1), Some(200));
        assert_eq!(tx.remove(&BalancesTable, &1), None);
        tx.commit();
    }

    #[test]
    fn writes_are_visible_within_the_tx() {
        let db = Database::open_in_memory();

        let tx = db.begin_write();
        tx.insert(&BalancesTable, &1, &100);
        assert_eq!(tx.get(&BalancesTable, &1), Some(100));

        let items = tx.iter(&BalancesTable, |r| r.collect::<Vec<_>>());
        assert_eq!(items, vec![(1, 100)]);
    }

    #[test]
    fn uncommitted_writes_are_discarded() {
        let db = Database::open_in_memory();

        let tx = db.begin_write();
        tx.insert(&UsersTable, &1, &"alice".to_string());
        drop(tx);

        let tx = db.begin_read();
        assert_eq!(tx.get(&UsersTable, &1), None);
    }

    #[test]
    fn reads_on_missing_tables_are_empty() {
        let db = Database::open_in_memory();

        let tx = db.begin_read();
        assert_eq!(tx.get(&UsersTable, &1), None);
        assert_eq!(tx.iter(&UsersTable, |r| r.collect::<Vec<_>>()), vec![]);
        assert_eq!(tx.prefix(&NotesTable, &1u64, |r| r.count()), 0);
        assert_eq!(tx.range(&BalancesTable, 0u64.., |r| r.count()), 0);
    }

    // Regression: a scan on a missing table must run the closure on an empty
    // iterator, not fabricate a default — `all` on an empty iterator is
    // `true`, while `bool::default()` is `false`.
    #[test]
    fn scans_on_missing_tables_run_the_closure_on_empty() {
        let db = Database::open_in_memory();

        let tx = db.begin_read();
        assert!(tx.iter(&NotesTable, |r| r.all(|entry| entry.0.0 > 0)));

        let tx = db.begin_write();
        assert!(tx.iter(&NotesTable, |r| r.all(|entry| entry.0.0 > 0)));
    }

    #[test]
    fn range_iterates_sorted() {
        let db = Database::open_in_memory();

        let tx = db.begin_write();
        for i in [7u64, 3, 9, 0, 5, 4, 6, 1, 8, 2] {
            tx.insert(&BalancesTable, &i, &(i * 10));
        }
        tx.commit();

        let tx = db.begin_read();
        let items = tx.range(&BalancesTable, 3u64..7u64, |r| r.collect::<Vec<_>>());

        assert_eq!(items, vec![(3, 30), (4, 40), (5, 50), (6, 60)]);
    }

    #[test]
    fn composite_range_uses_tuple_order() {
        let db = Database::open_in_memory();

        let tx = db.begin_write();
        for key in [
            (2u64, 0u32, 0u64),
            (1, 2, 5),
            (1, 2, 9),
            (1, 3, 0),
            (1, 1, 7),
        ] {
            tx.insert(&AddressTable, &key, &());
        }
        tx.commit();

        let tx = db.begin_read();
        let keys = tx.range(
            &AddressTable,
            (1u64, 2u32, u64::MIN)..=(1u64, 2u32, u64::MAX),
            |r| r.map(|entry| entry.0).collect::<Vec<_>>(),
        );

        assert_eq!(keys, vec![(1, 2, 5), (1, 2, 9)]);
    }

    #[test]
    fn prefix_scans_are_ordered_subsets() {
        let db = Database::open_in_memory();

        let tx = db.begin_write();
        for key in [
            (1u64, 1u32, "c".to_string()),
            (1, 1, "a".to_string()),
            (1, 2, "b".to_string()),
            (2, 1, "d".to_string()),
        ] {
            tx.insert(&NotesTable, &key, &());
        }
        tx.commit();

        let tx = db.begin_read();

        let fed = tx.prefix(&NotesTable, &1u64, |r| {
            r.map(|entry| entry.0).collect::<Vec<_>>()
        });
        assert_eq!(
            fed,
            vec![
                (1, 1, "a".to_string()),
                (1, 1, "c".to_string()),
                (1, 2, "b".to_string()),
            ]
        );

        let account = tx.prefix(&NotesTable, &(1u64, 1u32), |r| {
            r.map(|entry| entry.0).collect::<Vec<_>>()
        });
        assert_eq!(
            account,
            vec![(1, 1, "a".to_string()), (1, 1, "c".to_string())]
        );
    }

    #[test]
    fn prefix_rev_reverses_order() {
        let db = Database::open_in_memory();

        let tx = db.begin_write();
        for key in [(1u64, 1u32, 1u64), (1, 2, 2), (1, 3, 3), (2, 1, 4)] {
            tx.insert(&AddressTable, &key, &());
        }
        tx.commit();

        let tx = db.begin_read();

        let last = tx.prefix_rev(&AddressTable, &1u64, |r| r.next().map(|entry| entry.0));
        assert_eq!(last, Some((1, 3, 3)));

        let keys = tx.prefix_rev(&AddressTable, &1u64, |r| {
            r.map(|entry| entry.0).collect::<Vec<_>>()
        });
        assert_eq!(keys, vec![(1, 3, 3), (1, 2, 2), (1, 1, 1)]);
    }

    #[test]
    fn remove_prefix_removes_only_matching_rows() {
        let db = Database::open_in_memory();

        let tx = db.begin_write();
        for key in [(1u64, 1u32, 1u64), (1, 2, 2), (2, 1, 3)] {
            tx.insert(&AddressTable, &key, &());
        }
        tx.remove_prefix(&AddressTable, &1u64);
        tx.commit();

        let tx = db.begin_read();
        let keys = tx.iter(&AddressTable, |r| {
            r.map(|entry| entry.0).collect::<Vec<_>>()
        });

        assert_eq!(keys, vec![(2, 1, 3)]);
    }

    #[test]
    fn iter_rev_reverses_order() {
        let db = Database::open_in_memory();

        let tx = db.begin_write();
        for i in 0u64..4 {
            tx.insert(&BalancesTable, &i, &i);
        }
        tx.commit();

        let tx = db.begin_read();
        let keys = tx.iter_rev(&BalancesTable, |r| {
            r.map(|entry| entry.0).collect::<Vec<_>>()
        });

        assert_eq!(keys, vec![3, 2, 1, 0]);
    }

    #[test]
    fn clear_table_removes_all_rows() {
        let db = Database::open_in_memory();

        let tx = db.begin_write();
        tx.insert(&BalancesTable, &1, &100);
        tx.commit();

        let tx = db.begin_write();
        tx.clear_table(&BalancesTable);
        tx.commit();

        let tx = db.begin_read();
        assert_eq!(tx.get(&BalancesTable, &1), None);
    }

    // A reader that opened the table before a clear keeps its own MVCC
    // snapshot; fresh readers see the cleared (missing) table as empty.
    #[test]
    fn readers_survive_clear_table() {
        let db = Database::open_in_memory();

        let tx = db.begin_write();
        tx.insert(&BalancesTable, &1, &100);
        tx.commit();

        let reader = db.begin_read();
        assert_eq!(reader.iter(&BalancesTable, |r| r.count()), 1);

        let tx = db.begin_write();
        tx.clear_table(&BalancesTable);
        tx.commit();

        assert_eq!(reader.iter(&BalancesTable, |r| r.count()), 1);
        drop(reader);

        let reader = db.begin_read();
        assert_eq!(reader.iter(&BalancesTable, |r| r.count()), 0);
    }

    #[test]
    fn readers_see_a_stable_snapshot() {
        let db = Database::open_in_memory();

        let tx = db.begin_write();
        tx.insert(&BalancesTable, &1, &100);
        tx.commit();

        let reader = db.begin_read();
        assert_eq!(reader.get(&BalancesTable, &1), Some(100));

        let tx = db.begin_write();
        tx.insert(&BalancesTable, &1, &200);
        tx.commit();

        assert_eq!(reader.get(&BalancesTable, &1), Some(100));

        drop(reader);
        assert_eq!(db.begin_read().get(&BalancesTable, &1), Some(200));
    }

    #[test]
    fn second_writer_blocks_until_commit() {
        let db = Database::open_in_memory();

        let tx = db.begin_write();
        tx.insert(&BalancesTable, &1, &100);

        let db_second = db.clone();
        let second = std::thread::spawn(move || {
            let tx = db_second.begin_write();
            tx.insert(&BalancesTable, &2, &200);
            tx.commit();
        });

        std::thread::sleep(Duration::from_millis(50));
        tx.commit();
        second.join().unwrap();

        let tx = db.begin_read();
        assert_eq!(tx.get(&BalancesTable, &1), Some(100));
        assert_eq!(tx.get(&BalancesTable, &2), Some(200));
    }

    #[test]
    fn relaxed_writes_are_visible_and_flushed_by_durable_commits() {
        let db = Database::open_in_memory();

        let tx = db.begin_write_relaxed();
        tx.insert(&BalancesTable, &1, &100);
        tx.commit();

        let tx = db.begin_write();
        assert_eq!(tx.get(&BalancesTable, &1), Some(100));
        tx.insert(&BalancesTable, &2, &200);
        tx.commit();

        let tx = db.begin_read();
        assert_eq!(tx.get(&BalancesTable, &1), Some(100));
        assert_eq!(tx.get(&BalancesTable, &2), Some(200));
    }

    #[test]
    fn data_persists_across_reopen() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.redb");

        let db = Database::open(&path).unwrap();
        let tx = db.begin_write();
        tx.insert(&UsersTable, &1, &"alice".to_string());
        tx.commit();
        drop(db);

        let db = Database::open(&path).unwrap();
        let tx = db.begin_read();
        assert_eq!(tx.get(&UsersTable, &1), Some("alice".to_string()));
    }

    #[tokio::test]
    async fn wait_table_check_wakes_after_commit() {
        let db = Database::open_in_memory();

        let db_writer = db.clone();
        let writer = tokio::task::spawn_blocking(move || {
            std::thread::sleep(Duration::from_millis(50));
            let tx = db_writer.begin_write();
            tx.insert(&UsersTable, &1, &"alice".to_string());
            tx.commit();
        });

        let (value, _tx) = db
            .wait_table_check(&UsersTable, |tx| tx.get(&UsersTable, &1))
            .await;
        assert_eq!(value, "alice");

        writer.await.unwrap();
    }

    #[tokio::test]
    async fn wait_table_check_returns_consistent_tx() {
        let db = Database::open_in_memory();

        let tx = db.begin_write();
        tx.insert(&BalancesTable, &1, &50);
        tx.commit();

        let db_writer = db.clone();
        tokio::task::spawn_blocking(move || {
            std::thread::sleep(Duration::from_millis(50));
            let tx = db_writer.begin_write();
            tx.insert(&BalancesTable, &1, &150);
            tx.commit();
        });

        let (v, tx) = db
            .wait_table_check(&BalancesTable, |tx| {
                tx.get(&BalancesTable, &1).filter(|n| *n >= 100)
            })
            .await;

        assert_eq!(v, 150);
        assert_eq!(tx.get(&BalancesTable, &1), Some(150));
    }

    #[test]
    fn on_commit_fires_after_commit() {
        let db = Database::open_in_memory();
        let fired = Arc::new(AtomicBool::new(false));

        let tx = db.begin_write();
        tx.insert(&UsersTable, &1, &"alice".to_string());
        let f = fired.clone();
        tx.on_commit(move || f.store(true, Ordering::SeqCst));
        assert!(!fired.load(Ordering::SeqCst));
        tx.commit();

        assert!(fired.load(Ordering::SeqCst));
    }

    #[test]
    fn on_commit_does_not_fire_if_dropped() {
        let db = Database::open_in_memory();
        let fired = Arc::new(AtomicBool::new(false));

        let tx = db.begin_write();
        let f = fired.clone();
        tx.on_commit(move || f.store(true, Ordering::SeqCst));
        drop(tx);

        assert!(!fired.load(Ordering::SeqCst));
    }
}
