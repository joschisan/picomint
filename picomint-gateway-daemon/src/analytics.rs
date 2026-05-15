//! On-disk SQLite mirror of the gateway's gw-module event log.
//!
//! A single daemon-wide trailer task reads from the global event log and
//! `INSERT`s rows into `{DATA_DIR}/analytics.sqlite`. One table per event
//! kind plus a `payments` view that stitches sends/receives into a single
//! row per op. Federation_id is read directly off each event entry — every
//! event is federation-scoped at log time.
//!
//! The file is **wiped on every gateway startup** — analytics state is
//! derived, not authoritative. The event log in the gateway redb is the
//! source of truth; the trailer replays from position 0 on every boot.
//!
//! UsersTable and agents inspect the db directly via `sqlite3 analytics.sqlite`.
//! No query transport is layered on top.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use picomint_client::TxCreateEvent;
use picomint_client::gw::events::{
    ReceiveEvent, ReceiveFailureEvent, ReceiveRefundEvent, ReceiveSuccessEvent, SendCancelEvent,
    SendEvent, SendSuccessEvent,
};
use picomint_eventlog::{EventLogEntry, EventLogId};
use rusqlite::Connection;
use tokio::sync::Mutex;

use crate::AppState;

const CHUNK_SIZE: u64 = 10_000;

/// Sub-directory inside `DATA_DIR` that holds the SQLite analytics DB and
/// its WAL/SHM sidecar files. The whole directory is wiped on every
/// startup so we don't have to special-case individual files.
pub const ANALYTICS_DIR: &str = "analytics";
/// Filename of the analytics DB inside `ANALYTICS_DIR`.
pub const ANALYTICS_FILE: &str = "analytics.sqlite";

/// Shared handle to the analytics SQLite connection. All trailer tasks and
/// any future readers go through this single mutex-guarded connection — fine
/// because SQLite serializes writes internally anyway and our write volume
/// is bounded by event-log throughput.
#[derive(Clone)]
pub struct Analytics {
    conn: Arc<Mutex<Connection>>,
}

impl Analytics {
    /// Wipe `{DATA_DIR}/analytics/`, recreate it, and open a fresh SQLite
    /// DB with the schema + `payments` view installed. Analytics state is
    /// always rebuilt from the redb event log on startup, so we don't
    /// preserve anything across restarts.
    pub fn wipe_and_init(data_dir: &Path) -> anyhow::Result<Self> {
        let dir: PathBuf = data_dir.join(ANALYTICS_DIR);
        // A full directory wipe handles the db file and its WAL/SHM sidecars
        // in one shot.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).context("failed to create analytics dir")?;

        let conn = Connection::open(dir.join(ANALYTICS_FILE))
            .context("failed to open analytics.sqlite")?;
        // WAL mode: readers don't block the writer, writer doesn't block
        // readers. Critical for concurrent `sqlite3` CLI access while the
        // gateway is running.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        conn.execute_batch(SCHEMA_SQL)
            .context("failed to install analytics schema")?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }
}

/// Schema + `payments` view. Column naming matches the previous DataFusion
/// layout so operator-written queries and scripts keep working.
const SCHEMA_SQL: &str = r#"
CREATE TABLE send (
    operation  TEXT NOT NULL,
    ts            INTEGER NOT NULL,   -- msecs since unix epoch
    federation TEXT NOT NULL,
    outpoint      TEXT NOT NULL,
    amount_msat   INTEGER NOT NULL,
    ln_fee_msat   INTEGER NOT NULL,
    fee_msat      INTEGER NOT NULL,
    PRIMARY KEY (federation, operation)
);

CREATE TABLE send_success (
    operation  TEXT NOT NULL,
    ts            INTEGER NOT NULL,
    federation TEXT NOT NULL,
    preimage      TEXT NOT NULL,
    txid          TEXT NOT NULL,
    ln_fee_msat   INTEGER NOT NULL,
    PRIMARY KEY (federation, operation)
);

CREATE TABLE send_cancel (
    operation  TEXT NOT NULL,
    ts            INTEGER NOT NULL,
    federation TEXT NOT NULL,
    signature     TEXT NOT NULL,
    PRIMARY KEY (federation, operation)
);

CREATE TABLE receive (
    operation  TEXT NOT NULL,
    ts            INTEGER NOT NULL,
    federation TEXT NOT NULL,
    txid          TEXT NOT NULL,
    amount_msat   INTEGER NOT NULL,
    fee_msat      INTEGER NOT NULL,
    PRIMARY KEY (federation, operation)
);

CREATE TABLE receive_success (
    operation  TEXT NOT NULL,
    ts            INTEGER NOT NULL,
    federation TEXT NOT NULL,
    preimage      TEXT NOT NULL,
    PRIMARY KEY (federation, operation)
);

CREATE TABLE receive_failure (
    operation  TEXT NOT NULL,
    ts            INTEGER NOT NULL,
    federation TEXT NOT NULL,
    PRIMARY KEY (federation, operation)
);

CREATE TABLE receive_refund (
    operation  TEXT NOT NULL,
    ts            INTEGER NOT NULL,
    federation TEXT NOT NULL,
    txid          TEXT NOT NULL,
    PRIMARY KEY (federation, operation)
);

CREATE TABLE tx_create (
    operation     TEXT NOT NULL,
    ts            INTEGER NOT NULL,
    federation    TEXT NOT NULL,
    txid          TEXT NOT NULL,
    remint_msat   INTEGER NOT NULL,
    fee_msat      INTEGER NOT NULL,
    PRIMARY KEY (federation, operation)
);

CREATE INDEX idx_send_ts             ON send(ts);
CREATE INDEX idx_send_success_ts     ON send_success(ts);
CREATE INDEX idx_receive_ts          ON receive(ts);
CREATE INDEX idx_receive_success_ts  ON receive_success(ts);
CREATE INDEX idx_tx_create_ts        ON tx_create(ts);

CREATE VIEW payments AS
SELECT
    s.federation,
    s.operation,
    'outgoing' AS direction,
    s.ts AS started_at,
    COALESCE(succ.ts, canc.ts) AS completed_at,
    CASE
        WHEN succ.operation IS NOT NULL THEN 'success'
        WHEN canc.operation IS NOT NULL THEN 'cancelled'
        ELSE 'pending'
    END AS status,
    s.amount_msat,
    succ.preimage,
    tx.txid          AS tx_txid,
    tx.remint_msat   AS tx_remint_msat,
    tx.fee_msat      AS tx_fee_msat
FROM send s
LEFT JOIN send_success succ
       ON succ.federation = s.federation AND succ.operation = s.operation
LEFT JOIN send_cancel  canc
       ON canc.federation = s.federation AND canc.operation = s.operation
LEFT JOIN tx_create    tx
       ON tx.federation = s.federation AND tx.operation = s.operation
UNION ALL
SELECT
    r.federation,
    r.operation,
    'incoming' AS direction,
    r.ts AS started_at,
    COALESCE(succ.ts, fail.ts, refund.ts) AS completed_at,
    CASE
        WHEN succ.operation   IS NOT NULL THEN 'success'
        WHEN refund.operation IS NOT NULL THEN 'refunded'
        WHEN fail.operation   IS NOT NULL THEN 'failure'
        ELSE 'pending'
    END AS status,
    r.amount_msat,
    succ.preimage,
    tx.txid          AS tx_txid,
    tx.remint_msat   AS tx_remint_msat,
    tx.fee_msat      AS tx_fee_msat
FROM receive r
LEFT JOIN receive_success succ
       ON succ.federation = r.federation AND succ.operation = r.operation
LEFT JOIN receive_failure fail
       ON fail.federation = r.federation AND fail.operation = r.operation
LEFT JOIN receive_refund  refund
       ON refund.federation = r.federation AND refund.operation = r.operation
LEFT JOIN tx_create       tx
       ON tx.federation = r.federation AND tx.operation = r.operation;
"#;

/// Drain the global event log forward in chunks and mirror each gw event
/// into the SQLite analytics DB. Blocks on the global `event_notify` only
/// when caught up with the head. Spawned daemon-wide at startup.
pub async fn trailer(state: AppState) {
    let mut cursor = EventLogId::default();
    let notify = state.logger.event_notify(&state.gateway_db);

    loop {
        // Register interest in the next commit BEFORE reading, so we don't
        // miss a commit that lands between the read and `.await`.
        let notified = notify.notified();

        let chunk = state
            .logger
            .get_event_log(&state.gateway_db, cursor, CHUNK_SIZE);

        if let Some((last_id, _)) = chunk.last() {
            cursor = last_id.saturating_add(1);
            let entries: Vec<EventLogEntry> = chunk.iter().map(|(_, e)| e.clone()).collect();
            let analytics = state.analytics.clone();
            // rusqlite is sync — hop off the tokio runtime's thread pool for
            // the insert batch so we don't block other async work.
            if let Err(e) = tokio::task::spawn_blocking(move || insert_batch(&analytics, &entries))
                .await
                .expect("spawn_blocking join")
            {
                tracing::error!(
                    error = %e,
                    "analytics insert failed"
                );
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

fn insert_batch(analytics: &Analytics, entries: &[EventLogEntry]) -> anyhow::Result<()> {
    let mut guard = analytics.conn.blocking_lock();
    let tx = guard.transaction()?;
    for entry in entries {
        let operation = entry.operation.to_string();
        let ts = entry.timestamp as i64;
        let federation = entry.federation.to_string();
        if let Some(e) = entry.to_event::<SendEvent>() {
            tx.execute(
                "INSERT OR IGNORE INTO send \
                 (federation, operation, ts, outpoint, amount_msat, ln_fee_msat, fee_msat) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    federation,
                    operation,
                    ts,
                    format!("{}:{}", e.outpoint.txid, e.outpoint.out_idx),
                    e.amount.msats as i64,
                    e.ln_fee.msats as i64,
                    e.fee.msats as i64,
                ],
            )?;
        } else if let Some(e) = entry.to_event::<SendSuccessEvent>() {
            tx.execute(
                "INSERT OR IGNORE INTO send_success \
                 (federation, operation, ts, preimage, txid, ln_fee_msat) \
                 VALUES (?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    federation,
                    operation,
                    ts,
                    hex::encode(e.preimage),
                    e.txid.to_string(),
                    e.ln_fee.msats as i64,
                ],
            )?;
        } else if let Some(e) = entry.to_event::<SendCancelEvent>() {
            tx.execute(
                "INSERT OR IGNORE INTO send_cancel \
                 (federation, operation, ts, signature) VALUES (?, ?, ?, ?)",
                rusqlite::params![federation, operation, ts, e.signature.to_string()],
            )?;
        } else if let Some(e) = entry.to_event::<ReceiveEvent>() {
            tx.execute(
                "INSERT OR IGNORE INTO receive \
                 (federation, operation, ts, txid, amount_msat, fee_msat) \
                 VALUES (?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    federation,
                    operation,
                    ts,
                    e.txid.to_string(),
                    e.amount.msats as i64,
                    e.fee.msats as i64,
                ],
            )?;
        } else if let Some(e) = entry.to_event::<ReceiveSuccessEvent>() {
            tx.execute(
                "INSERT OR IGNORE INTO receive_success \
                 (federation, operation, ts, preimage) VALUES (?, ?, ?, ?)",
                rusqlite::params![federation, operation, ts, hex::encode(e.preimage)],
            )?;
        } else if entry.to_event::<ReceiveFailureEvent>().is_some() {
            tx.execute(
                "INSERT OR IGNORE INTO receive_failure \
                 (federation, operation, ts) VALUES (?, ?, ?)",
                rusqlite::params![federation, operation, ts],
            )?;
        } else if let Some(e) = entry.to_event::<ReceiveRefundEvent>() {
            tx.execute(
                "INSERT OR IGNORE INTO receive_refund \
                 (federation, operation, ts, txid) VALUES (?, ?, ?, ?)",
                rusqlite::params![federation, operation, ts, e.txid.to_string()],
            )?;
        } else if let Some(e) = entry.to_event::<TxCreateEvent>() {
            tx.execute(
                "INSERT OR IGNORE INTO tx_create \
                 (federation, operation, ts, txid, remint_msat, fee_msat) \
                 VALUES (?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    federation,
                    operation,
                    ts,
                    e.txid.to_string(),
                    e.remint.msats as i64,
                    e.fee.msats as i64,
                ],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}
