//! SQLite mirror of a client's event log, one table per event.
//!
//! A trailer task reads the daemon-wide event log forward and inserts
//! every entry into `{DATA_DIR}/analytics/analytics.sqlite`. Each event
//! type registered in [`events!`] gets a table named after its source and
//! kind — `gateway_send`, `core_tx_accept` — whose columns are the entry's
//! common fields followed by the event's own, as derived by
//! [`picomint_core::sql::SqlRow`]. There are no views: the schema is a
//! 1:1 translation of the log, and questions are asked of it in SQL.
//!
//! The file is **wiped on every startup**: analytics state is derived, not
//! authoritative. The event log in the daemon's database is the source of
//! truth, and the trailer replays it from position 0 on every boot, so
//! adding or renaming an event needs no migration.
//!
//! Operators and agents read the db through [`query`], read-only SQL
//! served by the daemon's admin CLI — the musl-static distroless image
//! ships no `sqlite3` binary, so nothing outside the daemon ever opens
//! the file.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use hex::ToHex as _;
use picomint_client::eventlog::{Event, EventLogEntry, EventLogId, EventSource};
use picomint_client::{Client, ecash, gateway, lightning, onchain};
use picomint_core::sql::{SqlRow, SqlValue};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, Transaction};
use serde_json::{Map, Value};
use tokio::sync::Mutex;
use tracing::{debug, error};

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

/// Every event the trailer mirrors. An event type missing here is logged
/// at debug level and skipped, so adding an event means adding it here.
macro_rules! events {
    ($($event:path),* $(,)?) => {
        fn schema() -> String {
            let mut sql = String::new();
            $(sql.push_str(&table_sql::<$event>());)*
            sql
        }

        fn insert(tx: &Transaction, id: EventLogId, entry: &EventLogEntry) -> anyhow::Result<bool> {
            $(if let Some(event) = entry.to_event::<$event>() {
                insert_row::<$event>(tx, id, entry, &event)?;

                return Ok(true);
            })*

            Ok(false)
        }
    };
}

events! {
    picomint_client::TxCreateEvent,
    picomint_client::TxAcceptEvent,
    picomint_client::TxRejectEvent,
    ecash::SendEvent,
    ecash::SendSuccessEvent,
    ecash::SendFailureEvent,
    ecash::ReissuanceEvent,
    ecash::ReceiveEvent,
    ecash::IssuanceSuccessEvent,
    ecash::IssuanceFailureEvent,
    onchain::events::SendEvent,
    onchain::events::SendSuccessEvent,
    onchain::events::SendFailureEvent,
    onchain::events::ReceiveEvent,
    lightning::events::SendEvent,
    lightning::events::SendSuccessEvent,
    lightning::events::SendRefundEvent,
    lightning::events::SendFailureEvent,
    lightning::events::ReceiveEvent,
    gateway::events::SendEvent,
    gateway::events::SendSuccessEvent,
    gateway::events::SendCancelEvent,
    gateway::events::ReceiveEvent,
    gateway::events::ReceiveSuccessEvent,
    gateway::events::ReceiveFailureEvent,
    gateway::events::ReceiveRefundEvent,
}

/// Shared handle to the analytics SQLite writer connection, used by the
/// trailer; readers (the `query` CLI) open their own read-only
/// connections. Mutex-guarded — fine because our write volume is bounded
/// by event-log throughput.
#[derive(Clone)]
pub struct Analytics {
    conn: Arc<Mutex<Connection>>,
}

impl Analytics {
    /// Wipe `{DATA_DIR}/analytics/`, recreate it, and open a fresh SQLite
    /// DB with one table per registered event. Analytics state is always
    /// rebuilt from the daemon-db event log on startup, so nothing is
    /// preserved across restarts.
    pub fn wipe_and_init(data_dir: &Path) -> anyhow::Result<Self> {
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

        conn.execute_batch(&schema())
            .context("failed to install analytics schema")?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }
}

/// Table name for an event: its source and kind, joined the way SQL
/// likes them — `gateway_send`, `core_tx_accept`.
pub fn table_name<E: Event>() -> String {
    let source = match E::SOURCE {
        EventSource::Core => "core",
        EventSource::Ecash => "ecash",
        EventSource::Onchain => "onchain",
        EventSource::Lightning => "lightning",
        EventSource::Gateway => "gateway",
    };

    format!("{source}_{}", E::KIND.to_string().replace('-', "_"))
}

/// The entry fields every table starts with, ahead of the event's own.
const COMMON_COLUMNS: [(&str, &str); 5] = [
    ("id", "INTEGER PRIMARY KEY"),
    ("ts", "INTEGER NOT NULL"),
    ("mint", "TEXT NOT NULL"),
    ("account", "TEXT NOT NULL"),
    ("operation", "TEXT NOT NULL"),
];

fn table_sql<E: Event + SqlRow>() -> String {
    let table = table_name::<E>();

    let columns = COMMON_COLUMNS
        .iter()
        .map(|column| format!("{} {}", column.0, column.1))
        .chain(
            E::columns()
                .iter()
                .map(|column| format!("{} {} NOT NULL", column.0, column.1)),
        )
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "CREATE TABLE {table} ({columns});\n\
         CREATE INDEX {table}_operation ON {table}(operation);\n\
         CREATE INDEX {table}_ts ON {table}(ts);\n"
    )
}

fn insert_row<E: Event + SqlRow>(
    tx: &Transaction,
    id: EventLogId,
    entry: &EventLogEntry,
    event: &E,
) -> anyhow::Result<()> {
    let placeholders = std::iter::repeat_n("?", COMMON_COLUMNS.len() + E::columns().len())
        .collect::<Vec<_>>()
        .join(", ");

    let mut values: Vec<rusqlite::types::Value> = vec![
        id.0.cast_signed().into(),
        entry.timestamp.cast_signed().into(),
        entry.mint.to_string().into(),
        format!("{:?}", entry.account).into(),
        entry.operation.to_string().into(),
    ];

    values.extend(event.values().into_iter().map(|value| match value {
        SqlValue::Integer(n) => n.into(),
        SqlValue::Text(s) => s.into(),
    }));

    tx.execute(
        &format!("INSERT INTO {} VALUES ({placeholders})", table_name::<E>()),
        rusqlite::params_from_iter(values),
    )?;

    Ok(())
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

/// Drain the event log forward in chunks and mirror every entry into the
/// SQLite db. Blocks on the client's `event_notify` only when caught up
/// with the head. Spawned daemon-wide at startup.
pub async fn trailer(client: Arc<Client>, analytics: Analytics) {
    let mut cursor = EventLogId::default();
    let notify = client.event_notify();

    loop {
        // Register interest in the next commit BEFORE reading, so we don't
        // miss a commit that lands between the read and `.await`.
        let notified = notify.notified();

        let chunk = client.get_event_log(cursor, CHUNK_SIZE);

        if let Some((last_id, _)) = chunk.last() {
            cursor = last_id.saturating_add(1);
            let entries = chunk.clone();
            let analytics = analytics.clone();
            // rusqlite is sync — hop off the tokio runtime's thread pool for
            // the insert batch so we don't block other async work.
            if let Err(e) = tokio::task::spawn_blocking(move || insert_batch(&analytics, &entries))
                .await
                .expect("spawn_blocking join")
            {
                error!(error = %e, "analytics insert failed");
            }
        }

        // Short chunk means we've caught up with the head; block until the
        // next commit. Full chunk means there might be more to drain — loop
        // without waiting.
        if (chunk.len() as u64) < CHUNK_SIZE {
            notified.await;
        }
    }
}

fn insert_batch(
    analytics: &Analytics,
    entries: &[(EventLogId, EventLogEntry)],
) -> anyhow::Result<()> {
    let mut guard = analytics.conn.blocking_lock();
    let tx = guard.transaction()?;

    for (id, entry) in entries {
        if !insert(&tx, *id, entry)? {
            debug!(kind = %entry.kind, source = ?entry.source, "event not registered for analytics");
        }
    }

    tx.commit()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every registered event's table installs, which is what catches a
    /// field type without a column mapping or two events sharing a name.
    #[test]
    fn schema_installs() {
        let conn = Connection::open_in_memory().expect("in-memory sqlite");

        let sql = schema();

        println!("{sql}");

        conn.execute_batch(&sql).expect("schema installs");
    }
}
