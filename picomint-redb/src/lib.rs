//! redb-backed database for picomint.
//!
//! Tables are typed value-like handles that implement the [`Table`] trait.
//! The [`table!`] macro declares a zero-sized unit struct implementing
//! [`Table`] with a fixed resolved name. Per-federation scoping is layered
//! on top by callers (see `picomint-client`'s `client_table!` macro): a
//! tuple struct `Name(FederationId)` implementing [`Table`] resolves to
//! `"{federation}/{label}"`. There is no namespace mode on [`Database`] or
//! transactions — the dbtx is always at root, so any caller holding a
//! single dbtx can write to global and per-federation tables in the same
//! atomic redb commit.
//!
//! Per-type encoding macros:
//!
//! - [`consensus_value!`] — implements `redb::Value` via consensus encoding.
//! - [`consensus_key!`] — implements `redb::Key` + `redb::Value` via consensus
//!   encoding with byte-lex compare. Byte-lex compare matches numeric /
//!   lexicographic order because our encoding is fixed-width big-endian for
//!   integers and raw bytes for hashes.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::marker::PhantomData;
use std::ops::RangeBounds;
use std::path::Path;
use std::sync::{Arc, Mutex};

pub use redb;
use redb::{ReadableDatabase, ReadableTable as _, TableDefinition};
use tokio::sync::Notify;

// ─── Per-type impl macros ─────────────────────────────────────────────────

/// Implement `redb::Value` for a type that already derives
/// `Encodable + Decodable`, serializing via consensus encoding.
///
/// ```ignore
/// #[derive(Debug, Encodable, Decodable)]
/// pub struct Foo { ... }
/// consensus_value!(Foo);
/// ```
#[macro_export]
macro_rules! consensus_value {
    ([$($g:tt)*] $ty:ty) => {
        impl<$($g)*> $crate::redb::Value for $ty {
            type SelfType<'a>
                = $ty
            where
                Self: 'a;

            type AsBytes<'a>
                = ::std::vec::Vec<u8>
            where
                Self: 'a;

            fn fixed_width() -> ::std::option::Option<usize> {
                None
            }

            fn from_bytes<'a>(data: &'a [u8]) -> Self
            where
                Self: 'a,
            {
                <$ty as ::picomint_encoding::Decodable>::consensus_decode(data)
                    .expect("consensus_decode_partial failed")
            }

            fn as_bytes<'a, 'b: 'a>(value: &'a Self) -> ::std::vec::Vec<u8>
            where
                Self: 'b,
            {
                <$ty as ::picomint_encoding::Encodable>::consensus_encode_to_vec(value)
            }

            fn type_name() -> $crate::redb::TypeName {
                $crate::redb::TypeName::new(concat!("picomint::", stringify!($ty)))
            }
        }
    };
    ($ty:ty) => {
        $crate::consensus_value!([] $ty);
    };
}

/// Implement `redb::Key + redb::Value` for a type that already derives
/// `Encodable + Decodable`, serializing via consensus encoding with byte-lex
/// `compare` (fine for set-style lookup tables where we never range over a
/// semantic ordering of K).
///
/// ```ignore
/// #[derive(Debug, Encodable, Decodable)]
/// pub struct Foo(...);
/// consensus_key!(Foo);
/// ```
#[macro_export]
macro_rules! consensus_key {
    ($ty:ty) => {
        $crate::consensus_value!($ty);

        impl $crate::redb::Key for $ty {
            fn compare(data1: &[u8], data2: &[u8]) -> ::std::cmp::Ordering {
                data1.cmp(data2)
            }
        }
    };
}

// ─── Table trait + table-declaring macros ────────────────────────────────

/// A typed table reference. Each implementor pairs `Key`/`Value` types with a
/// runtime [`resolved_name`](Self::resolved_name) that determines which on-disk
/// table the op should hit. Op methods on [`WriteTx`]/[`ReadTx`]/etc. accept
/// any `&T: Table` and dispatch via `resolved_name()`, so the dbtx itself
/// never needs a "namespace mode."
pub trait Table {
    type Key: redb::Key + 'static;
    type Value: redb::Value + 'static;

    /// On-disk table name.
    fn resolved_name(&self) -> String;
}

/// Type-erased carrier for any [`Table`] implementor. Holds a resolved
/// name plus the key/value type parameters, so callers can store a
/// table reference in a struct field (`TableDef<K, V>`) without pinning
/// the underlying concrete type. Build one from any `T: Table` via
/// [`TableDef::from`]; pass `&self.field` directly to dbtx op methods.
#[derive(Clone, Debug)]
pub struct TableDef<K, V> {
    name: String,
    _phantom: PhantomData<(K, V)>,
}

impl<K, V> TableDef<K, V>
where
    K: redb::Key + 'static,
    V: redb::Value + 'static,
{
    pub fn from<T: Table<Key = K, Value = V>>(t: T) -> Self {
        Self {
            name: t.resolved_name(),
            _phantom: PhantomData,
        }
    }
}

impl<K, V> Table for TableDef<K, V>
where
    K: redb::Key + 'static,
    V: redb::Value + 'static,
{
    type Key = K;
    type Value = V;

    fn resolved_name(&self) -> String {
        self.name.clone()
    }
}

/// Declare a daemon-global table. Expands to a zero-sized unit struct
/// implementing [`Table`] with a fixed resolved name.
///
/// ```ignore
/// table!(
///     UnixTimeVoteTable,
///     PeerId => u64,
///     "unix-time-vote",
/// );
/// ```
#[macro_export]
macro_rules! table {
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

            fn resolved_name(&self) -> ::std::string::String {
                $label.to_string()
            }
        }
    };
}

// ─── Database ────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Database {
    inner: Arc<DatabaseInner>,
}

struct DatabaseInner {
    env: redb::Database,
    /// Lazily-populated map of resolved table name -> shared `Notify`. Any
    /// commit that opened a table for write wakes every waiter on that table.
    notify: Mutex<BTreeMap<String, Arc<Notify>>>,
    /// Fires on every commit, regardless of which tables were written.
    global_notify: Arc<Notify>,
}

impl DatabaseInner {
    fn notify_for(&self, name: &str) -> Arc<Notify> {
        self.notify
            .lock()
            .expect("notify map poisoned")
            .entry(name.to_owned())
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone()
    }
}

impl Database {
    /// Open (or create) a redb database at `path`. The only fallible entry
    /// point; every other public method panics internally on redb errors.
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let env = redb::Database::create(path.as_ref())?;

        Ok(Self {
            inner: Arc::new(DatabaseInner {
                env,
                notify: Mutex::new(BTreeMap::new()),
                global_notify: Arc::new(Notify::new()),
            }),
        })
    }

    /// Open an in-memory database. Intended for tests and ephemeral dev use.
    pub fn open_in_memory() -> Self {
        let env = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .expect("in-memory redb create failed");

        Self {
            inner: Arc::new(DatabaseInner {
                env,
                notify: Mutex::new(BTreeMap::new()),
                global_notify: Arc::new(Notify::new()),
            }),
        }
    }

    pub fn begin_write(&self) -> WriteTx {
        let tx = self
            .inner
            .env
            .begin_write()
            .expect("redb begin_write failed");

        WriteTx {
            tx,
            db: self.inner.clone(),
            touched: Mutex::new(BTreeSet::new()),
            on_commit: Mutex::new(Vec::new()),
        }
    }

    pub fn begin_read(&self) -> ReadTx {
        let tx = self.inner.env.begin_read().expect("redb begin_read failed");

        ReadTx { tx }
    }

    /// Notification future for the next commit on this database. Fires on
    /// every committed write, regardless of which tables were touched.
    ///
    /// Must be constructed *before* the check it guards — tokio's `Notified`
    /// captures the `notify_waiters` generation at construction time, so any
    /// commit that happens after `wait_commit()` returns but before the
    /// future is awaited will still wake the waiter. Wrapping this in an
    /// `async fn` would defer the generation capture to first poll and
    /// reintroduce the TOCTOU race.
    pub fn wait_commit(&self) -> tokio::sync::futures::Notified<'_> {
        self.inner.global_notify.notified()
    }

    /// Shared [`Notify`] handle for `table`'s resolved name. Fires via
    /// `notify_waiters` on every commit that opened the table for write.
    /// Callers should construct `notified()` *before* the check it guards
    /// (see [`Self::wait_commit`]).
    pub fn notify_for_table<T: Table>(&self, def: &T) -> Arc<Notify> {
        self.inner.notify_for(&def.resolved_name())
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

pub struct ReadTx {
    tx: redb::ReadTransaction,
}

impl ReadTx {
    /// Borrow a view of this read tx.
    pub fn as_ref(&self) -> ReadTxRef<'_> {
        ReadTxRef { tx: &self.tx }
    }
}

/// Borrowed view of a [`ReadTx`].
pub struct ReadTxRef<'tx> {
    tx: &'tx redb::ReadTransaction,
}

pub struct WriteTx {
    tx: redb::WriteTransaction,
    db: Arc<DatabaseInner>,
    /// Resolved names of tables opened for write during this tx. Populated any
    /// time a table is opened (including for read), used to notify waiters on
    /// commit. Over-notifies on pure reads — harmless but slightly noisy.
    touched: Mutex<BTreeSet<String>>,
    on_commit: Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>,
}

impl WriteTx {
    /// Borrow a view of this write tx. The view cannot commit; only the
    /// owning [`WriteTx`] can.
    pub fn as_ref(&self) -> WriteTxRef<'_> {
        WriteTxRef {
            tx: &self.tx,
            touched: &self.touched,
            on_commit: &self.on_commit,
        }
    }

    /// Register a callback to run after a successful commit.
    pub fn on_commit(&self, f: impl FnOnce() + Send + 'static) {
        self.as_ref().on_commit(f);
    }

    pub fn commit(self) {
        let Self {
            tx,
            db,
            touched,
            on_commit,
            ..
        } = self;

        tx.commit().expect("redb commit failed");

        for name in touched.into_inner().expect("touched poisoned") {
            db.notify_for(&name).notify_waiters();
        }

        db.global_notify.notify_waiters();

        for cb in on_commit.into_inner().expect("on_commit poisoned") {
            cb();
        }
    }
}

/// Borrowed view of a [`WriteTx`]. This is what server modules and client
/// state machines receive; they cannot commit, but they can read, write, and
/// register post-commit callbacks that the owning [`WriteTx::commit`] will
/// fire.
pub struct WriteTxRef<'tx> {
    tx: &'tx redb::WriteTransaction,
    touched: &'tx Mutex<BTreeSet<String>>,
    on_commit: &'tx Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>,
}

impl<'tx> WriteTxRef<'tx> {
    /// Register a callback to run after a successful commit.
    pub fn on_commit(&self, f: impl FnOnce() + Send + 'static) {
        self.on_commit
            .lock()
            .expect("on_commit poisoned")
            .push(Box::new(f));
    }

    /// Open the native redb table at `def`'s resolved name and hand it to
    /// `f`. Registers the table in `touched` for post-commit notification.
    pub(crate) fn with_native_table<D, R>(
        &self,
        def: &D,
        f: impl FnOnce(&mut redb::Table<'_, D::Key, D::Value>) -> R,
    ) -> R
    where
        D: Table,
    {
        let resolved = def.resolved_name();
        let td: TableDefinition<D::Key, D::Value> = TableDefinition::new(&resolved);
        let mut table = self
            .tx
            .open_table(td)
            .expect("redb open_table (write) failed");
        let r = f(&mut table);
        drop(table);
        self.touched
            .lock()
            .expect("touched poisoned")
            .insert(resolved);
        r
    }
}

impl ReadTxRef<'_> {
    /// Open the native redb table and hand it to `f`. Returns `None` if the
    /// table has never been written — treat that as "empty" at the call site.
    pub(crate) fn with_native_table<D, R>(
        &self,
        def: &D,
        f: impl FnOnce(&redb::ReadOnlyTable<D::Key, D::Value>) -> R,
    ) -> Option<R>
    where
        D: Table,
    {
        let resolved = def.resolved_name();
        let td: TableDefinition<D::Key, D::Value> = TableDefinition::new(&resolved);
        match self.tx.open_table(td) {
            Ok(t) => Some(f(&t)),
            Err(redb::TableError::TableDoesNotExist(_)) => None,
            Err(e) => panic!("redb open_table (read) failed: {e}"),
        }
    }
}

// ─── Ergonomic typed ops over Table ──────────────────────────────────────
//
// HRTB `T: for<'a> redb::Value<SelfType<'a> = T>` is the "owned" contract —
// call sites hand in and receive back owned `T`, not borrowed slices.
// Satisfied by any type built with `consensus_value!` / `consensus_key!`
// and tuples thereof.

/// Closure-scoped iterator over a redb table. Wraps [`redb::Range`] so callers
/// receive owned `(K, V)` pairs directly — decoding and error unwrapping happen
/// here so that call sites aren't peppered with `.expect(...)` / `.value()`.
pub struct RedbIter<'a, WK, K, WV, V>
where
    WK: for<'b> redb::Key<SelfType<'b> = K> + 'static,
    WV: for<'b> redb::Value<SelfType<'b> = V> + 'static,
{
    inner: redb::Range<'a, WK, WV>,
}

impl<'a, WK, K, WV, V> Iterator for RedbIter<'a, WK, K, WV, V>
where
    WK: for<'b> redb::Key<SelfType<'b> = K> + 'static,
    WV: for<'b> redb::Value<SelfType<'b> = V> + 'static,
{
    type Item = (K, V);

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|r| {
            let (k, v) = r.expect("redb iter item failed");
            (k.value(), v.value())
        })
    }
}

impl<'a, WK, K, WV, V> DoubleEndedIterator for RedbIter<'a, WK, K, WV, V>
where
    WK: for<'b> redb::Key<SelfType<'b> = K> + 'static,
    WV: for<'b> redb::Value<SelfType<'b> = V> + 'static,
{
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|r| {
            let (k, v) = r.expect("redb iter item failed");
            (k.value(), v.value())
        })
    }
}

impl<'tx> WriteTxRef<'tx> {
    pub fn insert<D, K, V>(&self, def: &D, key: &K, value: &V) -> Option<V>
    where
        D: Table,
        D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
        D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
        K: Debug + 'static,
        V: Debug + 'static,
    {
        self.with_native_table(def, |t| {
            t.insert(key, value)
                .expect("redb insert failed")
                .map(|g| g.value())
        })
    }

    pub fn remove<D, K, V>(&self, def: &D, key: &K) -> Option<V>
    where
        D: Table,
        D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
        D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
        K: Debug + 'static,
        V: Debug + 'static,
    {
        self.with_native_table(def, |t| {
            t.remove(key)
                .expect("redb remove failed")
                .map(|g| g.value())
        })
    }

    pub fn delete_table<D: Table>(&self, def: &D) {
        let resolved = def.resolved_name();
        let td: TableDefinition<D::Key, D::Value> = TableDefinition::new(&resolved);
        match self.tx.delete_table(td) {
            Ok(_) => {}
            Err(redb::TableError::TableDoesNotExist(_)) => {}
            Err(e) => panic!("redb delete_table failed: {e}"),
        }
        self.touched
            .lock()
            .expect("touched poisoned")
            .insert(resolved);
    }

    pub fn get<D, K, V>(&self, def: &D, key: &K) -> Option<V>
    where
        D: Table,
        D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
        D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
        K: Debug + 'static,
        V: Debug + 'static,
    {
        self.with_native_table(def, |t| {
            t.get(key).expect("redb get failed").map(|g| g.value())
        })
    }

    pub fn iter<D, K, V, R>(
        &self,
        def: &D,
        f: impl FnOnce(&mut RedbIter<'_, D::Key, K, D::Value, V>) -> R,
    ) -> R
    where
        D: Table,
        D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
        D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
        K: 'static,
        V: 'static,
    {
        self.with_native_table(def, |t| {
            let mut iter = RedbIter {
                inner: t.iter().expect("redb iter failed"),
            };
            f(&mut iter)
        })
    }

    pub fn range<D, K, V, B, R>(
        &self,
        def: &D,
        range: B,
        f: impl FnOnce(&mut RedbIter<'_, D::Key, K, D::Value, V>) -> R,
    ) -> R
    where
        D: Table,
        D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
        D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
        K: 'static,
        V: 'static,
        B: RangeBounds<K>,
    {
        self.with_native_table(def, |t| {
            let mut iter = RedbIter {
                inner: t
                    .range::<&K>((range.start_bound(), range.end_bound()))
                    .expect("redb range failed"),
            };
            f(&mut iter)
        })
    }
}

impl ReadTxRef<'_> {
    pub fn get<D, K, V>(&self, def: &D, key: &K) -> Option<V>
    where
        D: Table,
        D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
        D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
        K: Debug + 'static,
        V: Debug + 'static,
    {
        self.with_native_table(def, |t| {
            t.get(key).expect("redb get failed").map(|g| g.value())
        })
        .flatten()
    }

    pub fn iter<D, K, V, R>(
        &self,
        def: &D,
        f: impl FnOnce(&mut RedbIter<'_, D::Key, K, D::Value, V>) -> R,
    ) -> R
    where
        D: Table,
        D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
        D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
        K: 'static,
        V: 'static,
        R: Default,
    {
        self.with_native_table(def, |t| {
            let mut iter = RedbIter {
                inner: t.iter().expect("redb iter failed"),
            };
            f(&mut iter)
        })
        .unwrap_or_default()
    }

    pub fn range<D, K, V, B, R>(
        &self,
        def: &D,
        range: B,
        f: impl FnOnce(&mut RedbIter<'_, D::Key, K, D::Value, V>) -> R,
    ) -> R
    where
        D: Table,
        D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
        D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
        K: 'static,
        V: 'static,
        B: RangeBounds<K>,
        R: Default,
    {
        self.with_native_table(def, |t| {
            let mut iter = RedbIter {
                inner: t
                    .range::<&K>((range.start_bound(), range.end_bound()))
                    .expect("redb range failed"),
            };
            f(&mut iter)
        })
        .unwrap_or_default()
    }
}

// ─── DbRead / DbWrite trait abstraction ──────────────────────────────────
//
// Successor to the old `IReadDatabaseTransactionOps` trait tower. Typed
// methods are defined directly over `Table`-implementing tables and
// implemented on each concrete tx type. Server modules take `&impl DbRead` /
// `&impl DbWrite` to stay generic over owned-vs-borrowed and read-vs-write.

pub trait DbRead {
    fn get<D, K, V>(&self, def: &D, key: &K) -> Option<V>
    where
        D: Table,
        D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
        D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
        K: Debug + 'static,
        V: Debug + 'static;

    fn iter<D, K, V, R>(
        &self,
        def: &D,
        f: impl FnOnce(&mut RedbIter<'_, D::Key, K, D::Value, V>) -> R,
    ) -> R
    where
        D: Table,
        D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
        D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
        K: 'static,
        V: 'static,
        R: Default;

    fn range<D, K, V, B, R>(
        &self,
        def: &D,
        range: B,
        f: impl FnOnce(&mut RedbIter<'_, D::Key, K, D::Value, V>) -> R,
    ) -> R
    where
        D: Table,
        D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
        D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
        K: 'static,
        V: 'static,
        B: RangeBounds<K>,
        R: Default;
}

pub trait DbWrite: DbRead {
    fn insert<D, K, V>(&self, def: &D, key: &K, value: &V) -> Option<V>
    where
        D: Table,
        D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
        D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
        K: Debug + 'static,
        V: Debug + 'static;

    fn remove<D, K, V>(&self, def: &D, key: &K) -> Option<V>
    where
        D: Table,
        D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
        D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
        K: Debug + 'static,
        V: Debug + 'static;

    fn delete_table<D: Table>(&self, def: &D);
}

// Macro to cut the read + write method-body duplication per type.
// Each impl delegates to inherent methods that carry the actual logic.
macro_rules! impl_db_read_via_inherent {
    ($ty:ty) => {
        impl DbRead for $ty {
            fn get<D, K, V>(&self, def: &D, key: &K) -> Option<V>
            where
                D: Table,
                D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
                D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
                K: Debug + 'static,
                V: Debug + 'static,
            {
                <$ty>::get(self, def, key)
            }

            fn iter<D, K, V, R>(
                &self,
                def: &D,
                f: impl FnOnce(&mut RedbIter<'_, D::Key, K, D::Value, V>) -> R,
            ) -> R
            where
                D: Table,
                D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
                D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
                K: 'static,
                V: 'static,
                R: Default,
            {
                <$ty>::iter(self, def, f)
            }

            fn range<D, K, V, B, R>(
                &self,
                def: &D,
                range: B,
                f: impl FnOnce(&mut RedbIter<'_, D::Key, K, D::Value, V>) -> R,
            ) -> R
            where
                D: Table,
                D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
                D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
                K: 'static,
                V: 'static,
                B: RangeBounds<K>,
                R: Default,
            {
                <$ty>::range(self, def, range, f)
            }
        }
    };
}

macro_rules! impl_db_write_via_inherent {
    ($ty:ty) => {
        impl DbWrite for $ty {
            fn insert<D, K, V>(&self, def: &D, key: &K, value: &V) -> Option<V>
            where
                D: Table,
                D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
                D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
                K: Debug + 'static,
                V: Debug + 'static,
            {
                <$ty>::insert(self, def, key, value)
            }

            fn remove<D, K, V>(&self, def: &D, key: &K) -> Option<V>
            where
                D: Table,
                D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
                D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
                K: Debug + 'static,
                V: Debug + 'static,
            {
                <$ty>::remove(self, def, key)
            }

            fn delete_table<D: Table>(&self, def: &D) {
                <$ty>::delete_table(self, def)
            }
        }
    };
}

// ─── Owned-tx delegation ─────────────────────────────────────────────────
//
// UsersTable commonly call `.insert/.get/...` directly on the owned
// `WriteTx`/`ReadTx` (not just on the borrowed
// `WriteTxRef`/`ReadTxRef`). These inherent impls delegate via `.as_ref()`.

impl ReadTx {
    pub fn get<D, K, V>(&self, def: &D, key: &K) -> Option<V>
    where
        D: Table,
        D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
        D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
        K: Debug + 'static,
        V: Debug + 'static,
    {
        self.as_ref().get(def, key)
    }

    pub fn iter<D, K, V, R>(
        &self,
        def: &D,
        f: impl FnOnce(&mut RedbIter<'_, D::Key, K, D::Value, V>) -> R,
    ) -> R
    where
        D: Table,
        D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
        D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
        K: 'static,
        V: 'static,
        R: Default,
    {
        self.as_ref().iter(def, f)
    }

    pub fn range<D, K, V, B, R>(
        &self,
        def: &D,
        range: B,
        f: impl FnOnce(&mut RedbIter<'_, D::Key, K, D::Value, V>) -> R,
    ) -> R
    where
        D: Table,
        D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
        D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
        K: 'static,
        V: 'static,
        B: RangeBounds<K>,
        R: Default,
    {
        self.as_ref().range(def, range, f)
    }
}

impl WriteTx {
    pub fn insert<D, K, V>(&self, def: &D, key: &K, value: &V) -> Option<V>
    where
        D: Table,
        D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
        D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
        K: Debug + 'static,
        V: Debug + 'static,
    {
        self.as_ref().insert(def, key, value)
    }

    pub fn remove<D, K, V>(&self, def: &D, key: &K) -> Option<V>
    where
        D: Table,
        D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
        D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
        K: Debug + 'static,
        V: Debug + 'static,
    {
        self.as_ref().remove(def, key)
    }

    pub fn delete_table<D: Table>(&self, def: &D) {
        self.as_ref().delete_table(def)
    }

    pub fn get<D, K, V>(&self, def: &D, key: &K) -> Option<V>
    where
        D: Table,
        D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
        D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
        K: Debug + 'static,
        V: Debug + 'static,
    {
        self.as_ref().get(def, key)
    }

    pub fn iter<D, K, V, R>(
        &self,
        def: &D,
        f: impl FnOnce(&mut RedbIter<'_, D::Key, K, D::Value, V>) -> R,
    ) -> R
    where
        D: Table,
        D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
        D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
        K: 'static,
        V: 'static,
        R: Default,
    {
        self.as_ref().iter(def, f)
    }

    pub fn range<D, K, V, B, R>(
        &self,
        def: &D,
        range: B,
        f: impl FnOnce(&mut RedbIter<'_, D::Key, K, D::Value, V>) -> R,
    ) -> R
    where
        D: Table,
        D::Key: for<'a> redb::Key<SelfType<'a> = K> + 'static,
        D::Value: for<'a> redb::Value<SelfType<'a> = V> + 'static,
        K: 'static,
        V: 'static,
        B: RangeBounds<K>,
        R: Default,
    {
        self.as_ref().range(def, range, f)
    }
}

impl_db_read_via_inherent!(ReadTxRef<'_>);
impl_db_read_via_inherent!(WriteTxRef<'_>);
impl_db_read_via_inherent!(ReadTx);
impl_db_read_via_inherent!(WriteTx);
impl_db_write_via_inherent!(WriteTxRef<'_>);
impl_db_write_via_inherent!(WriteTx);

// ─── Playground tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    table!(UsersTable, u64 => String, "users");
    table!(BalancesTable, u64 => u64, "balances");

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
    fn uncommitted_writes_are_discarded() {
        let db = Database::open_in_memory();

        let tx = db.begin_write();
        tx.insert(&UsersTable, &1, &"alice".to_string());
        drop(tx);

        let tx = db.begin_read();
        assert_eq!(tx.get(&UsersTable, &1), None);
    }

    #[test]
    fn range_iterates_sorted() {
        let db = Database::open_in_memory();

        let tx = db.begin_write();
        for i in 0u64..10 {
            tx.insert(&BalancesTable, &i, &(i * 10));
        }
        tx.commit();

        let tx = db.begin_read();
        let items = tx.range(&BalancesTable, 3u64..7u64, |r| r.collect::<Vec<_>>());

        assert_eq!(items, vec![(3, 30), (4, 40), (5, 50), (6, 60)]);
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
        use std::sync::atomic::{AtomicBool, Ordering};

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
        use std::sync::atomic::{AtomicBool, Ordering};

        let db = Database::open_in_memory();
        let fired = Arc::new(AtomicBool::new(false));

        let tx = db.begin_write();
        let f = fired.clone();
        tx.on_commit(move || f.store(true, Ordering::SeqCst));
        drop(tx);

        assert!(!fired.load(Ordering::SeqCst));
    }
}
