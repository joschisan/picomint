//! SQLite mirror of an append-only log, one table per row type.
//!
//! An embedder — the client daemon, the gateway daemon, the mint node —
//! keeps an ordered log in its database that is the source of truth for
//! what happened: the client's event log, the node's accepted consensus
//! items. A trailer task reads that log forward and inserts every entry
//! into `{DATA_DIR}/analytics/analytics.sqlite`, exploded into the rows
//! the embedder's registry derives for it with
//! [`picomint_core::sql::SqlRow`]. There are no views: the schema is a
//! 1:1 translation of the log, and questions are asked of it in SQL.
//!
//! The file is **wiped on every startup**: analytics state is derived, not
//! authoritative. The trailer replays the log from its start on every boot,
//! so adding or renaming a table needs no migration.
//!
//! Operators and agents read the db through [`query`], read-only SQL
//! served by the embedder's admin CLI — the distroless image ships no
//! `sqlite3` binary, so nothing outside the daemon ever opens the file.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use hex::ToHex as _;
use picomint_core::sql::{Row, SqlValue};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, Transaction};
use serde_json::{Map, Value};
use tokio::sync::{Mutex, Notify};
use tracing::error;

const CHUNK_SIZE: u64 = 10_000;

/// Sub-directory inside `DATA_DIR` that holds the SQLite analytics DB and
/// its WAL/SHM sidecar files. The whole directory is wiped on every
/// startup so we don't have to special-case individual files.
pub const ANALYTICS_DIR: &str = "analytics";
/// Filename of the analytics DB inside `ANALYTICS_DIR`.
pub const ANALYTICS_FILE: &str = "analytics.sqlite";

/// One JSON object per row, keyed by result column name — the same shape
/// `sqlite3 --json` prints.
pub type Rows = Vec<Map<String, Value>>;

/// Shared handle to the analytics SQLite writer connection, used by the
/// trailer; readers (the `query` CLI) open their own read-only
/// connections. Mutex-guarded — fine because our write volume is bounded
/// by log throughput.
#[derive(Clone)]
pub struct Analytics {
    conn: Arc<Mutex<Connection>>,
}

impl Analytics {
    /// Wipe `{DATA_DIR}/analytics/`, recreate it, and open a fresh SQLite
    /// DB with `schema` installed. Analytics state is always rebuilt from
    /// the log on startup, so nothing is preserved across restarts.
    pub fn wipe_and_init(data_dir: &Path, schema: &str) -> anyhow::Result<Self> {
        let dir: PathBuf = data_dir.join(ANALYTICS_DIR);
        // A full directory wipe handles the db file and its WAL/SHM sidecars
        // in one shot.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).context("failed to create analytics dir")?;

        let conn = Connection::open(dir.join(ANALYTICS_FILE))
            .context("failed to open analytics.sqlite")?;
        // WAL mode: readers don't block the writer, so a CLI query never
        // waits behind a trailer insert batch. Synchronous is off because
        // durability is worthless for state we wipe on every boot anyway.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "OFF")?;

        conn.execute_batch(schema)
            .context("failed to install analytics schema")?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }
}

/// Run read-only SQL against the analytics db and return one JSON object per
/// row, keyed by result column name. Opens its own `SQLITE_OPEN_READ_ONLY`
/// connection — WAL mode lets it read concurrently with the trailer's writer
/// connection, and the flag rejects any write statement outright.
pub fn query(data_dir: &Path, sql: &str) -> anyhow::Result<Rows> {
    let path = data_dir.join(ANALYTICS_DIR).join(ANALYTICS_FILE);

    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .context("failed to open analytics.sqlite read-only")?;

    let mut statement = conn.prepare(sql)?;

    let columns = statement
        .column_names()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    let mut rows = statement.query([])?;
    let mut result = Vec::new();

    while let Some(row) = rows.next()? {
        let mut object = Map::new();

        for (i, column) in columns.iter().enumerate() {
            let value = match row.get_ref(i)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(n) => Value::from(n),
                ValueRef::Real(f) => Value::from(f),
                ValueRef::Text(text) => String::from_utf8_lossy(text).into_owned().into(),
                ValueRef::Blob(blob) => Value::String(blob.encode_hex()),
            };

            object.insert(column.clone(), value);
        }

        result.push(object);
    }

    Ok(result)
}

/// Drain a log forward in chunks and mirror every entry into the SQLite
/// db. `next_chunk` returns up to the given number of entries past the
/// ones it returned before, `rows` explodes an entry into the rows it
/// lands as. Blocks on `notify` only when caught up with the head — the
/// embedder passes the notify that fires on every commit to the log's
/// table. Spawned daemon-wide at startup.
pub async fn trailer<E: Send + 'static>(
    analytics: Analytics,
    notify: Arc<Notify>,
    mut next_chunk: impl FnMut(u64) -> Vec<E>,
    rows: impl Fn(&E) -> Vec<Row> + Clone + Send + 'static,
) {
    loop {
        // Register interest in the next commit BEFORE reading, so we don't
        // miss a commit that lands between the read and `.await`.
        let notified = notify.notified();

        let chunk = next_chunk(CHUNK_SIZE);

        let len = chunk.len() as u64;

        if len > 0 {
            let analytics = analytics.clone();
            let rows = rows.clone();
            // rusqlite is sync — hop off the tokio runtime's thread pool for
            // the insert batch so we don't block other async work.
            if let Err(e) =
                tokio::task::spawn_blocking(move || insert_batch(&analytics, &chunk, &rows))
                    .await
                    .expect("spawn_blocking join")
            {
                error!(error = %e, "analytics insert failed");
            }
        }

        // Short chunk means we've caught up with the head; block until the
        // next commit. Full chunk means there might be more to drain — loop
        // without waiting.
        if len < CHUNK_SIZE {
            notified.await;
        }
    }
}

fn insert_batch<E>(
    analytics: &Analytics,
    entries: &[E],
    rows: &impl Fn(&E) -> Vec<Row>,
) -> anyhow::Result<()> {
    let mut guard = analytics.conn.blocking_lock();
    let tx = guard.transaction()?;

    for entry in entries {
        for row in rows(entry) {
            insert_row(&tx, row)?;
        }
    }

    tx.commit()?;

    Ok(())
}

fn insert_row(tx: &Transaction, row: Row) -> anyhow::Result<()> {
    let placeholders = std::iter::repeat_n("?", row.values.len())
        .collect::<Vec<_>>()
        .join(", ");

    let values = row.values.into_iter().map(|value| match value {
        SqlValue::Null => rusqlite::types::Value::Null,
        SqlValue::Integer(n) => n.into(),
        SqlValue::Text(s) => s.into(),
    });

    // Cached per statement text, so a rebuild of a long log prepares each
    // table's insert once rather than once per row.
    tx.prepare_cached(&format!(
        "INSERT INTO {} VALUES ({placeholders})",
        row.table
    ))?
    .execute(rusqlite::params_from_iter(values))?;

    Ok(())
}
