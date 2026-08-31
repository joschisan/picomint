//! SQLite-backed database for picomint.
//!
//! Tables are typed handles implementing the [`Table`] trait, declared with
//! the [`table!`] macro. Every table maps to one SQLite table with one BLOB
//! column per key element (`k0..kn`) and a single BLOB value column (`v`),
//! all serialized via consensus encoding. Multi-element keys are declared as
//! tuples; the macro additionally implements [`Prefix`] for every proper
//! prefix of the key tuple, backing indexed prefix scans and deletes.
//!
//! No SQL and no rusqlite type appears in this crate's public API — callers
//! interact exclusively through typed tables, keys, values and prefixes.
//!
//! Ordering: SQLite compares BLOB columns with memcmp, and consensus encoding
//! is fixed-width big-endian for integers and raw bytes for hashes, so byte
//! order per column matches the elements' logical order. Iteration, prefix
//! and range order therefore equal element-wise tuple order. (This differs
//! from comparing the concatenated encoding only for variable-length key
//! elements, where element-wise is the semantically correct tuple order.)
//!
//! Concurrency mirrors the redb model: a single write transaction at a time
//! (one writer connection behind a checkout) and snapshot-isolated readers
//! (WAL mode, one pooled connection per read transaction). A read snapshot
//! is established at the transaction's first read, not at [`Database::begin_read`].

use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;
use std::mem;
use std::ops::{Bound, RangeBounds};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use picomint_encoding::{Decodable, Encodable};
use rusqlite::{CachedStatement, Connection, Row, params_from_iter};
use tokio::sync::Notify;

const READER_POOL_LIMIT: usize = 4;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Consensus-encode a single key element or value to its column bytes.
/// Public for use by [`table!`] expansions; not intended for direct calls.
pub fn encode<T: Encodable>(value: &T) -> Vec<u8> {
    value.consensus_encode_to_vec()
}

/// Decode a single key element or value from its column bytes.
/// Public for use by [`table!`] expansions; not intended for direct calls.
pub fn decode<T: Decodable>(bytes: &[u8]) -> T {
    T::consensus_decode(bytes).expect("stored column bytes failed to decode")
}

// ─── Table trait + Prefix + table-declaring macro ────────────────────────

/// A typed table reference. Implementors pair key/value types with a fixed
/// on-disk table name and the per-column key encoding. Implemented by the
/// [`table!`] macro — the key encodes to [`Self::KEY_COLS`] separate columns
/// so that prefixes and ranges compile to indexed `WHERE` clauses.
pub trait Table {
    type Key;
    type Value: Encodable + Decodable;

    /// Number of key columns.
    const KEY_COLS: usize;

    /// On-disk table name.
    fn name(&self) -> &'static str;

    /// Encode the key into one byte string per key column.
    fn encode_key(key: &Self::Key) -> Vec<Vec<u8>>;

    /// Decode the key from one byte string per key column.
    fn decode_key(cols: &[&[u8]]) -> Self::Key;
}

/// A typed proper prefix of `D`'s key tuple. Implemented by the [`table!`]
/// macro for the bare first element and every longer proper prefix tuple,
/// e.g. a `(A, B, C)` key yields impls for `A` and `(A, B)`.
pub trait Prefix<D: Table> {
    /// Number of leading key columns this prefix constrains.
    const LEN: usize;

    /// Encode the prefix into one byte string per constrained column.
    fn encode_prefix(&self) -> Vec<Vec<u8>>;
}

/// Declare a table. Expands to a zero-sized unit struct implementing
/// [`Table`], plus [`Prefix`] impls for every proper prefix of a tuple key.
///
/// ```ignore
/// table!(
///     NoteTable,
///     (FederationId, Account, SpendableNote) => (),
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

            const KEY_COLS: usize = 3;

            fn name(&self) -> &'static str {
                $label
            }

            fn encode_key(key: &Self::Key) -> ::std::vec::Vec<::std::vec::Vec<u8>> {
                ::std::vec![
                    $crate::encode(&key.0),
                    $crate::encode(&key.1),
                    $crate::encode(&key.2),
                ]
            }

            fn decode_key(cols: &[&[u8]]) -> Self::Key {
                (
                    $crate::decode(cols[0]),
                    $crate::decode(cols[1]),
                    $crate::decode(cols[2]),
                )
            }
        }

        impl $crate::Prefix<$name> for $k0 {
            const LEN: usize = 1;

            fn encode_prefix(&self) -> ::std::vec::Vec<::std::vec::Vec<u8>> {
                ::std::vec![$crate::encode(self)]
            }
        }

        impl $crate::Prefix<$name> for ($k0, $k1) {
            const LEN: usize = 2;

            fn encode_prefix(&self) -> ::std::vec::Vec<::std::vec::Vec<u8>> {
                ::std::vec![$crate::encode(&self.0), $crate::encode(&self.1)]
            }
        }
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

            const KEY_COLS: usize = 2;

            fn name(&self) -> &'static str {
                $label
            }

            fn encode_key(key: &Self::Key) -> ::std::vec::Vec<::std::vec::Vec<u8>> {
                ::std::vec![$crate::encode(&key.0), $crate::encode(&key.1)]
            }

            fn decode_key(cols: &[&[u8]]) -> Self::Key {
                ($crate::decode(cols[0]), $crate::decode(cols[1]))
            }
        }

        impl $crate::Prefix<$name> for $k0 {
            const LEN: usize = 1;

            fn encode_prefix(&self) -> ::std::vec::Vec<::std::vec::Vec<u8>> {
                ::std::vec![$crate::encode(self)]
            }
        }
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

            const KEY_COLS: usize = 1;

            fn name(&self) -> &'static str {
                $label
            }

            fn encode_key(key: &Self::Key) -> ::std::vec::Vec<::std::vec::Vec<u8>> {
                ::std::vec![$crate::encode(key)]
            }

            fn decode_key(cols: &[&[u8]]) -> Self::Key {
                $crate::decode(cols[0])
            }
        }
    };
}

// ─── SQL generation ──────────────────────────────────────────────────────
//
// All SQL in the codebase is generated here, from a table's name and key
// column count. Statements are compiled once per connection via rusqlite's
// prepared-statement cache, keyed by the generated string.

fn col_list(n: usize) -> String {
    (0..n)
        .map(|i| format!("k{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn where_key(n: usize) -> String {
    (0..n)
        .map(|i| format!("k{i} = ?"))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn order_list(n: usize, desc: bool) -> String {
    let direction = if desc { "DESC" } else { "ASC" };

    (0..n)
        .map(|i| format!("k{i} {direction}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn key_tuple(n: usize) -> String {
    match n {
        1 => "k0".to_string(),
        n => format!("({})", col_list(n)),
    }
}

fn placeholder_tuple(n: usize) -> String {
    match n {
        1 => "?".to_string(),
        n => format!("({})", vec!["?"; n].join(", ")),
    }
}

fn create_sql(name: &str, n: usize) -> String {
    let cols: String = (0..n).map(|i| format!("k{i} BLOB NOT NULL, ")).collect();

    format!(
        "CREATE TABLE IF NOT EXISTS \"{name}\" ({cols}v BLOB NOT NULL, PRIMARY KEY ({})) WITHOUT ROWID",
        col_list(n)
    )
}

fn select_sql(name: &str, n: usize) -> String {
    format!("SELECT v FROM \"{name}\" WHERE {}", where_key(n))
}

fn upsert_sql(name: &str, n: usize) -> String {
    format!(
        "INSERT INTO \"{name}\" ({}, v) VALUES ({}, ?) ON CONFLICT({}) DO UPDATE SET v = excluded.v",
        col_list(n),
        vec!["?"; n].join(", "),
        col_list(n),
    )
}

fn delete_sql(name: &str, n: usize) -> String {
    format!("DELETE FROM \"{name}\" WHERE {}", where_key(n))
}

fn clear_sql(name: &str) -> String {
    format!("DELETE FROM \"{name}\"")
}

fn scan_sql(name: &str, n: usize, clauses: &[String], desc: bool) -> String {
    let mut sql = format!("SELECT {}, v FROM \"{name}\"", col_list(n));

    if !clauses.is_empty() {
        sql.push_str(&format!(" WHERE {}", clauses.join(" AND ")));
    }

    sql.push_str(&format!(" ORDER BY {}", order_list(n, desc)));

    sql
}

/// Row-value comparison clauses and parameters for a typed key range.
fn range_clauses<D: Table>(range: &impl RangeBounds<D::Key>) -> (Vec<String>, Vec<Vec<u8>>) {
    let mut clauses = Vec::new();
    let mut params = Vec::new();

    let mut bound = |bound: Bound<&D::Key>, op: &str, op_strict: &str| match bound {
        Bound::Included(key) => {
            clauses.push(format!(
                "{} {op} {}",
                key_tuple(D::KEY_COLS),
                placeholder_tuple(D::KEY_COLS)
            ));
            params.extend(D::encode_key(key));
        }
        Bound::Excluded(key) => {
            clauses.push(format!(
                "{} {op_strict} {}",
                key_tuple(D::KEY_COLS),
                placeholder_tuple(D::KEY_COLS)
            ));
            params.extend(D::encode_key(key));
        }
        Bound::Unbounded => {}
    };

    bound(range.start_bound(), ">=", ">");
    bound(range.end_bound(), "<=", "<");

    (clauses, params)
}

// ─── Database ────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Database {
    inner: Arc<DatabaseInner>,
}

struct DatabaseInner {
    path: PathBuf,
    /// The single writer connection, absent while checked out by a
    /// [`WriteTx`]. Checkout blocks on `writer_returned` — SQLite allows
    /// one write transaction at a time, so this also serializes writers.
    writer: Mutex<Option<Connection>>,
    writer_returned: Condvar,
    readers: Mutex<Vec<Connection>>,
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

    fn checkout_writer(&self) -> Connection {
        let mut guard = self.writer.lock().expect("writer poisoned");

        loop {
            if let Some(conn) = guard.take() {
                return conn;
            }

            guard = self.writer_returned.wait(guard).expect("writer poisoned");
        }
    }

    fn checkin_writer(&self, conn: Connection) {
        *self.writer.lock().expect("writer poisoned") = Some(conn);

        self.writer_returned.notify_one();
    }

    fn open_writer(&self) -> Connection {
        let conn = Connection::open(&self.path).expect("sqlite open failed");

        conn.busy_timeout(BUSY_TIMEOUT)
            .expect("sqlite busy_timeout failed");

        conn.pragma_update(None, "synchronous", "FULL")
            .expect("sqlite synchronous pragma failed");

        conn
    }

    fn checkout_reader(&self) -> Connection {
        let pooled = self.readers.lock().expect("readers poisoned").pop();

        pooled.unwrap_or_else(|| {
            let conn = Connection::open(&self.path).expect("sqlite open failed");

            conn.busy_timeout(BUSY_TIMEOUT)
                .expect("sqlite busy_timeout failed");

            conn
        })
    }

    fn checkin_reader(&self, conn: Connection) {
        let mut pool = self.readers.lock().expect("readers poisoned");

        if pool.len() < READER_POOL_LIMIT {
            pool.push(conn);
        }
    }
}

impl Database {
    /// Open (or create) a SQLite database at `path`. The only fallible entry
    /// point; every other public method panics internally on SQLite errors.
    ///
    /// There is no in-memory mode: every `:memory:` connection gets its own
    /// private database, and this handle opens several (one writer plus
    /// pooled readers). Tests open a database in a temporary directory.
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref().to_path_buf();

        let conn = Connection::open(&path)?;

        conn.busy_timeout(BUSY_TIMEOUT)?;

        // WAL is persistent in the database file; setting it once on the
        // writer covers every later connection. The pragma returns the new
        // mode as a row, so it cannot go through `pragma_update`.
        let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;

        conn.pragma_update(None, "synchronous", "FULL")?;

        Ok(Self {
            inner: Arc::new(DatabaseInner {
                path,
                writer: Mutex::new(Some(conn)),
                writer_returned: Condvar::new(),
                readers: Mutex::new(Vec::new()),
                notify: Mutex::new(BTreeMap::new()),
            }),
        })
    }

    pub fn begin_write(&self) -> WriteTx {
        self.begin_write_inner(false)
    }

    /// Begin a write transaction whose commit does **not** fsync. The writes
    /// become durable only when a later [`Self::begin_write`] commit flushes
    /// the WAL, and a crash rolls back to that last durable commit. Use only
    /// for state a node can reconstruct after a crash (e.g. BFT units
    /// re-fetched from peers) — never for money-bearing or safety-critical
    /// writes.
    pub fn begin_write_relaxed(&self) -> WriteTx {
        self.begin_write_inner(true)
    }

    fn begin_write_inner(&self, relaxed: bool) -> WriteTx {
        let conn = self.inner.checkout_writer();

        if relaxed {
            // In WAL mode NORMAL skips the per-commit fsync; the commit is
            // atomic but only becomes durable when a later FULL commit (or a
            // checkpoint) syncs the log. This reproduces redb's
            // `Durability::None` without risking file corruption like OFF.
            conn.pragma_update(None, "synchronous", "NORMAL")
                .expect("sqlite synchronous pragma failed");
        }

        conn.execute_batch("BEGIN IMMEDIATE")
            .expect("sqlite begin failed");

        WriteTx {
            conn: Mutex::new(Some(conn)),
            relaxed,
            db: self.inner.clone(),
            touched: Mutex::new(BTreeSet::new()),
            on_commit: Mutex::new(Vec::new()),
        }
    }

    pub fn begin_read(&self) -> ReadTx {
        let conn = self.inner.checkout_reader();

        conn.execute_batch("BEGIN").expect("sqlite begin failed");

        ReadTx {
            conn: Mutex::new(Some(conn)),
            db: self.inner.clone(),
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

// The tx types must be `Sync` (redb's were): callers hold `&WriteTx` /
// `&ReadTx` across `.await`, which requires `Sync` for the future to be
// `Send`. `Connection` is `Send` but not `Sync`, so each op takes a short
// internal lock. Consequence: ops must not be called re-entrantly from
// inside an iteration closure on the same tx — collect first, then act.

pub struct WriteTx {
    /// Present until commit or drop returns the connection to the pool.
    conn: Mutex<Option<Connection>>,
    relaxed: bool,
    db: Arc<DatabaseInner>,
    /// Names of tables written during this tx, used to notify waiters on
    /// commit.
    touched: Mutex<BTreeSet<&'static str>>,
    on_commit: Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>,
}

pub struct ReadTx {
    /// Present until drop returns the connection to the pool.
    conn: Mutex<Option<Connection>>,
    db: Arc<DatabaseInner>,
}

impl WriteTx {
    /// Register a callback to run after a successful commit.
    pub fn on_commit(&self, f: impl FnOnce() + Send + 'static) {
        self.on_commit
            .lock()
            .expect("on_commit poisoned")
            .push(Box::new(f));
    }

    pub fn commit(mut self) {
        let conn = self
            .conn
            .get_mut()
            .expect("connection poisoned")
            .take()
            .expect("connection present until commit");

        conn.execute_batch("COMMIT").expect("sqlite commit failed");

        if self.relaxed {
            conn.pragma_update(None, "synchronous", "FULL")
                .expect("sqlite synchronous pragma failed");
        }

        self.db.checkin_writer(conn);

        let touched = mem::take(&mut *self.touched.lock().expect("touched poisoned"));

        for name in touched {
            self.db.notify_for(name).notify_waiters();
        }

        let callbacks = mem::take(&mut *self.on_commit.lock().expect("on_commit poisoned"));

        for cb in callbacks {
            cb();
        }
    }

    /// Idempotently create the backing SQLite table. Runs inside the write
    /// tx, so a rollback also rolls the creation back — mirroring redb,
    /// where any write-tx table open creates the table.
    ///
    /// The schema is append-only: tables are created here but never dropped
    /// ([`Self::clear_table`] deletes rows, not the table). This is
    /// load-bearing — cached prepared statements on *other* connections
    /// lazily re-prepare against the live schema on their next step, and a
    /// dropped table would turn that into a mid-iteration error.
    fn ensure_table<D: Table>(&self, def: &D) {
        self.conn()
            .prepare_cached(&create_sql(def.name(), D::KEY_COLS))
            .expect("sqlite prepare failed")
            .execute([])
            .expect("sqlite create table failed");
    }

    fn touch<D: Table>(&self, def: &D) {
        self.touched
            .lock()
            .expect("touched poisoned")
            .insert(def.name());
    }
}

impl Drop for WriteTx {
    fn drop(&mut self) {
        // Recover from poisoning rather than panic — a panic in a destructor
        // during unwind aborts the process, and poisoning only means some op
        // panicked mid-flight; the ROLLBACK below deals with any half-done
        // statement, and the fallback replaces the connection entirely.
        let conn = self
            .conn
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();

        let Some(conn) = conn else {
            return;
        };

        // Never panic in drop: if the connection cannot be restored to a
        // clean state, replace it with a fresh one so writers don't block
        // forever on checkout.
        let clean = conn.execute_batch("ROLLBACK").is_ok()
            && (!self.relaxed || conn.pragma_update(None, "synchronous", "FULL").is_ok());

        match clean {
            true => self.db.checkin_writer(conn),
            false => self.db.checkin_writer(self.db.open_writer()),
        }
    }
}

impl Drop for ReadTx {
    fn drop(&mut self) {
        // See `WriteTx::drop` on poison recovery; a reader that fails to end
        // its tx is simply not returned to the pool.
        let conn = self
            .conn
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();

        let Some(conn) = conn else {
            return;
        };

        if conn.execute_batch("COMMIT").is_ok() {
            self.db.checkin_reader(conn);
        }
    }
}

// ─── Typed ops ───────────────────────────────────────────────────────────

/// Streaming iterator over a table scan. Wraps the underlying SQLite rows so
/// callers receive owned, decoded `(K, V)` pairs directly. A table that has
/// never been created iterates as empty (`rows` is `None`).
pub struct SqliteIter<'a, D: Table> {
    rows: Option<rusqlite::Rows<'a>>,
    _table: PhantomData<D>,
}

impl<D: Table> Iterator for SqliteIter<'_, D> {
    type Item = (D::Key, D::Value);

    fn next(&mut self) -> Option<Self::Item> {
        self.rows
            .as_mut()?
            .next()
            .expect("sqlite row read failed")
            .map(decode_row::<D>)
    }
}

fn decode_row<D: Table>(row: &Row) -> (D::Key, D::Value) {
    let key_cols: Vec<Vec<u8>> = (0..D::KEY_COLS)
        .map(|i| row.get(i).expect("sqlite key column read failed"))
        .collect();

    let key_refs: Vec<&[u8]> = key_cols.iter().map(|col| col.as_slice()).collect();

    let value: Vec<u8> = row
        .get(D::KEY_COLS)
        .expect("sqlite value column read failed");

    (D::decode_key(&key_refs), decode(&value))
}

/// Prepare a read statement, returning `None` if the backing table has never
/// been created — callers treat that as "empty".
fn prepare_read<'a>(conn: &'a Connection, sql: &str) -> Option<CachedStatement<'a>> {
    match conn.prepare_cached(sql) {
        Ok(stmt) => Some(stmt),
        Err(rusqlite::Error::SqliteFailure(_, Some(message)))
            if message.contains("no such table") =>
        {
            None
        }
        Err(e) => panic!("sqlite prepare failed: {e}"),
    }
}

fn query_value<V: Decodable>(conn: &Connection, sql: &str, params: &[Vec<u8>]) -> Option<V> {
    let mut stmt = prepare_read(conn, sql)?;

    let mut rows = stmt
        .query(params_from_iter(params.iter()))
        .expect("sqlite query failed");

    rows.next().expect("sqlite row read failed").map(|row| {
        decode(
            &row.get::<_, Vec<u8>>(0)
                .expect("sqlite value column read failed"),
        )
    })
}

/// Run `f` over the decoded rows of `sql`. A backing table that has never
/// been created scans as empty — `f` always runs, so closure results on an
/// empty iterator (e.g. `all` returning `true`) hold either way.
fn scan<D: Table, R>(
    conn: &Connection,
    sql: &str,
    params: Vec<Vec<u8>>,
    f: impl FnOnce(&mut SqliteIter<'_, D>) -> R,
) -> R {
    let Some(mut stmt) = prepare_read(conn, sql) else {
        return f(&mut SqliteIter {
            rows: None,
            _table: PhantomData,
        });
    };

    let rows = stmt
        .query(params_from_iter(params))
        .expect("sqlite query failed");

    let mut iter = SqliteIter {
        rows: Some(rows),
        _table: PhantomData,
    };

    f(&mut iter)
}

impl WriteTx {
    /// Insert `value` under `key`, returning the previously stored value.
    pub fn insert<D: Table>(&self, def: &D, key: &D::Key, value: &D::Value) -> Option<D::Value> {
        self.ensure_table(def);

        self.touch(def);

        let key_cols = D::encode_key(key);

        let previous = query_value(
            &self.conn(),
            &select_sql(def.name(), D::KEY_COLS),
            &key_cols,
        );

        let params = key_cols.into_iter().chain([encode(value)]);

        self.conn()
            .prepare_cached(&upsert_sql(def.name(), D::KEY_COLS))
            .expect("sqlite prepare failed")
            .execute(params_from_iter(params))
            .expect("sqlite upsert failed");

        previous
    }

    /// Remove the entry under `key`, returning the previously stored value.
    pub fn remove<D: Table>(&self, def: &D, key: &D::Key) -> Option<D::Value> {
        self.ensure_table(def);

        self.touch(def);

        let key_cols = D::encode_key(key);

        let previous = query_value(
            &self.conn(),
            &select_sql(def.name(), D::KEY_COLS),
            &key_cols,
        );

        self.conn()
            .prepare_cached(&delete_sql(def.name(), D::KEY_COLS))
            .expect("sqlite prepare failed")
            .execute(params_from_iter(key_cols.iter()))
            .expect("sqlite delete failed");

        previous
    }

    /// Remove every entry whose key starts with `prefix`. Used to clear one
    /// scope's slice of a shared table, e.g. a federation's rows on leave.
    pub fn remove_prefix<D: Table, P: Prefix<D>>(&self, def: &D, prefix: &P) {
        self.ensure_table(def);

        self.touch(def);

        self.conn()
            .prepare_cached(&delete_sql(def.name(), P::LEN))
            .expect("sqlite prepare failed")
            .execute(params_from_iter(prefix.encode_prefix()))
            .expect("sqlite delete failed");
    }

    /// Remove every entry of the table. The table itself stays in the
    /// schema — see [`Self::ensure_table`] on why it must never be dropped.
    pub fn clear_table<D: Table>(&self, def: &D) {
        self.ensure_table(def);

        self.touch(def);

        self.conn()
            .prepare_cached(&clear_sql(def.name()))
            .expect("sqlite prepare failed")
            .execute([])
            .expect("sqlite delete failed");
    }
}

// ─── DbRead / DbWrite trait abstraction ──────────────────────────────────
//
// Typed methods defined directly over `Table`-implementing tables. The read
// methods are provided once here over the sealed connection accessor, so
// both tx types share one implementation. Server modules take
// `&impl DbRead` / `&impl DbWrite` to stay generic over owned-vs-borrowed
// and read-vs-write.

mod sealed {
    use std::sync::MutexGuard;

    use rusqlite::Connection;

    /// Locked view of a tx's connection; derefs to [`Connection`] for the
    /// duration of one op.
    pub struct ConnGuard<'a>(pub(super) MutexGuard<'a, Option<Connection>>);

    impl std::ops::Deref for ConnGuard<'_> {
        type Target = Connection;

        fn deref(&self) -> &Connection {
            self.0
                .as_ref()
                .expect("connection present until commit or drop")
        }
    }

    /// Sealed supertrait of [`DbRead`](super::DbRead): restricts the trait
    /// to the two tx types and hands their connection to the provided read
    /// methods. Unnameable outside the crate, so `conn` stays internal.
    pub trait HasConn {
        fn conn(&self) -> ConnGuard<'_>;
    }
}

use sealed::{ConnGuard, HasConn};

impl HasConn for WriteTx {
    fn conn(&self) -> ConnGuard<'_> {
        ConnGuard(self.conn.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl HasConn for ReadTx {
    fn conn(&self) -> ConnGuard<'_> {
        ConnGuard(self.conn.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Typed read operations, shared by both tx types. A table that has never
/// been written reads as empty — `None` for gets, an empty iteration for
/// scans — so reads never create tables as a side effect.
pub trait DbRead: HasConn {
    fn get<D: Table>(&self, def: &D, key: &D::Key) -> Option<D::Value> {
        query_value(
            &self.conn(),
            &select_sql(def.name(), D::KEY_COLS),
            &D::encode_key(key),
        )
    }

    fn iter<D: Table, R>(&self, def: &D, f: impl FnOnce(&mut SqliteIter<'_, D>) -> R) -> R {
        let sql = scan_sql(def.name(), D::KEY_COLS, &[], false);

        scan(&self.conn(), &sql, Vec::new(), f)
    }

    /// Iterate the table in descending key order.
    fn iter_rev<D: Table, R>(&self, def: &D, f: impl FnOnce(&mut SqliteIter<'_, D>) -> R) -> R {
        let sql = scan_sql(def.name(), D::KEY_COLS, &[], true);

        scan(&self.conn(), &sql, Vec::new(), f)
    }

    /// Iterate every entry whose key starts with `prefix`, in ascending key
    /// order. Compiles to an indexed `WHERE` on the prefix columns.
    fn prefix<D: Table, P: Prefix<D>, R>(
        &self,
        def: &D,
        prefix: &P,
        f: impl FnOnce(&mut SqliteIter<'_, D>) -> R,
    ) -> R {
        let sql = scan_sql(def.name(), D::KEY_COLS, &[where_key(P::LEN)], false);

        scan(&self.conn(), &sql, prefix.encode_prefix(), f)
    }

    /// Iterate every entry whose key starts with `prefix`, in descending key
    /// order.
    fn prefix_rev<D: Table, P: Prefix<D>, R>(
        &self,
        def: &D,
        prefix: &P,
        f: impl FnOnce(&mut SqliteIter<'_, D>) -> R,
    ) -> R {
        let sql = scan_sql(def.name(), D::KEY_COLS, &[where_key(P::LEN)], true);

        scan(&self.conn(), &sql, prefix.encode_prefix(), f)
    }

    /// Iterate the entries within a typed key range, in ascending key order.
    fn range<D: Table, B: RangeBounds<D::Key>, R>(
        &self,
        def: &D,
        range: B,
        f: impl FnOnce(&mut SqliteIter<'_, D>) -> R,
    ) -> R {
        let (clauses, params) = range_clauses::<D>(&range);

        let sql = scan_sql(def.name(), D::KEY_COLS, &clauses, false);

        scan(&self.conn(), &sql, params, f)
    }
}

impl DbRead for ReadTx {}
impl DbRead for WriteTx {}

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

    /// A database in a fresh temporary directory. The dir must be held
    /// alive alongside the handle — dropping it deletes the files.
    fn test_db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::TempDir::new().expect("failed to create temp dir");

        let db = Database::open(dir.path().join("test.sqlite")).expect("sqlite open failed");

        (dir, db)
    }

    #[test]
    fn basic_read_write() {
        let (_dir, db) = test_db();

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
        let (_dir, db) = test_db();

        let tx = db.begin_write();
        assert_eq!(tx.insert(&BalancesTable, &1, &100), None);
        assert_eq!(tx.insert(&BalancesTable, &1, &200), Some(100));
        assert_eq!(tx.remove(&BalancesTable, &1), Some(200));
        assert_eq!(tx.remove(&BalancesTable, &1), None);
        tx.commit();
    }

    #[test]
    fn writes_are_visible_within_the_tx() {
        let (_dir, db) = test_db();

        let tx = db.begin_write();
        tx.insert(&BalancesTable, &1, &100);
        assert_eq!(tx.get(&BalancesTable, &1), Some(100));

        let items = tx.iter(&BalancesTable, |r| r.collect::<Vec<_>>());
        assert_eq!(items, vec![(1, 100)]);
    }

    #[test]
    fn uncommitted_writes_are_discarded() {
        let (_dir, db) = test_db();

        let tx = db.begin_write();
        tx.insert(&UsersTable, &1, &"alice".to_string());
        drop(tx);

        let tx = db.begin_read();
        assert_eq!(tx.get(&UsersTable, &1), None);
    }

    #[test]
    fn reads_on_missing_tables_are_empty() {
        let (_dir, db) = test_db();

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
        let (_dir, db) = test_db();

        let tx = db.begin_read();
        assert!(tx.iter(&NotesTable, |r| r.all(|entry| entry.0.0 > 0)));

        let tx = db.begin_write();
        assert!(tx.iter(&NotesTable, |r| r.all(|entry| entry.0.0 > 0)));
    }

    #[test]
    fn range_iterates_sorted() {
        let (_dir, db) = test_db();

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
        let (_dir, db) = test_db();

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
        let (_dir, db) = test_db();

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
        let (_dir, db) = test_db();

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
        let (_dir, db) = test_db();

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
        let (_dir, db) = test_db();

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
        let (_dir, db) = test_db();

        let tx = db.begin_write();
        tx.insert(&BalancesTable, &1, &100);
        tx.commit();

        let tx = db.begin_write();
        tx.clear_table(&BalancesTable);
        tx.commit();

        let tx = db.begin_read();
        assert_eq!(tx.get(&BalancesTable, &1), None);
    }

    // Regression: a reader connection caches its scan statement; clearing
    // the table must not invalidate it. When clear_table was a DROP TABLE,
    // the cached statement's lazy re-prepare on its next step failed with
    // "no such table" mid-iteration and killed the process.
    #[test]
    fn cached_statements_survive_clear_table() {
        let (_dir, db) = test_db();

        let tx = db.begin_write();
        tx.insert(&BalancesTable, &1, &100);
        tx.commit();

        let reader = db.begin_read();
        assert_eq!(reader.iter(&BalancesTable, |r| r.count()), 1);
        drop(reader);

        let tx = db.begin_write();
        tx.clear_table(&BalancesTable);
        tx.commit();

        let reader = db.begin_read();
        assert_eq!(reader.iter(&BalancesTable, |r| r.count()), 0);
    }

    #[test]
    fn readers_see_a_stable_snapshot() {
        let (_dir, db) = test_db();

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
        let (_dir, db) = test_db();

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
        let (_dir, db) = test_db();

        let tx = db.begin_write_relaxed();
        tx.insert(&BalancesTable, &1, &100);
        tx.commit();

        let tx = db.begin_write();
        tx.insert(&BalancesTable, &2, &200);
        tx.commit();

        let tx = db.begin_read();
        assert_eq!(tx.get(&BalancesTable, &1), Some(100));
        assert_eq!(tx.get(&BalancesTable, &2), Some(200));
    }

    #[test]
    fn data_persists_across_reopen() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.sqlite");

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
        let (_dir, db) = test_db();

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
        let (_dir, db) = test_db();

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
        let (_dir, db) = test_db();
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
        let (_dir, db) = test_db();
        let fired = Arc::new(AtomicBool::new(false));

        let tx = db.begin_write();
        let f = fired.clone();
        tx.on_commit(move || f.store(true, Ordering::SeqCst));
        drop(tx);

        assert!(!fired.load(Ordering::SeqCst));
    }
}
