//! On-disk SQLite mirror of the gateway's gw-module event log.
//!
//! Per-federation trailer tasks read from the client event log and `INSERT`
//! rows into `{DATA_DIR}/analytics.sqlite`. One table per event kind plus a
//! `payments` view that stitches sends/receives into a single row per op.
//!
//! The file is **wiped on every gateway startup** — analytics state is
//! derived, not authoritative. The event log in each client's redb is the
//! source of truth; the trailer replays from position 0 on every boot.
//!
//! Users and agents inspect the db directly via `sqlite3 analytics.sqlite`.
//! No query transport is layered on top.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use picomint_client::Client;
use picomint_client::gw::events::{
    ReceiveEvent, ReceiveFailureEvent, ReceiveRefundEvent, ReceiveSuccessEvent, SendCancelEvent,
    SendEvent, SendSuccessEvent,
};
use picomint_core::config::FederationId;
use picomint_core::task::TaskGroup;
use picomint_eventlog::{EventLogEntry, EventLogId};
use rusqlite::Connection;
use tokio::sync::Mutex;

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
    operation_id  TEXT NOT NULL,
    ts            INTEGER NOT NULL,   -- usecs since unix epoch
    federation_id TEXT NOT NULL,
    outpoint      TEXT NOT NULL,
    amount_msat   INTEGER NOT NULL,
    ln_fee_msat   INTEGER NOT NULL,
    fee_msat      INTEGER NOT NULL,
    PRIMARY KEY (federation_id, operation_id)
);

CREATE TABLE send_success (
    operation_id  TEXT NOT NULL,
    ts            INTEGER NOT NULL,
    federation_id TEXT NOT NULL,
    preimage      TEXT NOT NULL,
    txid          TEXT NOT NULL,
    ln_fee_msat   INTEGER NOT NULL,
    PRIMARY KEY (federation_id, operation_id)
);

CREATE TABLE send_cancel (
    operation_id  TEXT NOT NULL,
    ts            INTEGER NOT NULL,
    federation_id TEXT NOT NULL,
    signature     TEXT NOT NULL,
    PRIMARY KEY (federation_id, operation_id)
);

CREATE TABLE receive (
    operation_id  TEXT NOT NULL,
    ts            INTEGER NOT NULL,
    federation_id TEXT NOT NULL,
    txid          TEXT NOT NULL,
    amount_msat   INTEGER NOT NULL,
    fee_msat      INTEGER NOT NULL,
    PRIMARY KEY (federation_id, operation_id)
);

CREATE TABLE receive_success (
    operation_id  TEXT NOT NULL,
    ts            INTEGER NOT NULL,
    federation_id TEXT NOT NULL,
    preimage      TEXT NOT NULL,
    PRIMARY KEY (federation_id, operation_id)
);

CREATE TABLE receive_failure (
    operation_id  TEXT NOT NULL,
    ts            INTEGER NOT NULL,
    federation_id TEXT NOT NULL,
    PRIMARY KEY (federation_id, operation_id)
);

CREATE TABLE receive_refund (
    operation_id  TEXT NOT NULL,
    ts            INTEGER NOT NULL,
    federation_id TEXT NOT NULL,
    txid          TEXT NOT NULL,
    PRIMARY KEY (federation_id, operation_id)
);

CREATE INDEX idx_send_ts             ON send(ts);
CREATE INDEX idx_send_success_ts     ON send_success(ts);
CREATE INDEX idx_receive_ts          ON receive(ts);
CREATE INDEX idx_receive_success_ts  ON receive_success(ts);

CREATE VIEW payments AS
SELECT
    s.federation_id,
    s.operation_id,
    'outgoing' AS direction,
    s.ts AS started_at,
    COALESCE(succ.ts, canc.ts) AS completed_at,
    CASE
        WHEN succ.operation_id IS NOT NULL THEN 'success'
        WHEN canc.operation_id IS NOT NULL THEN 'cancelled'
        ELSE 'pending'
    END AS status,
    s.amount_msat,
    succ.preimage
FROM send s
LEFT JOIN send_success succ
       ON succ.federation_id = s.federation_id AND succ.operation_id = s.operation_id
LEFT JOIN send_cancel  canc
       ON canc.federation_id = s.federation_id AND canc.operation_id = s.operation_id
UNION ALL
SELECT
    r.federation_id,
    r.operation_id,
    'incoming' AS direction,
    r.ts AS started_at,
    COALESCE(succ.ts, fail.ts, refund.ts) AS completed_at,
    CASE
        WHEN succ.operation_id   IS NOT NULL THEN 'success'
        WHEN refund.operation_id IS NOT NULL THEN 'refunded'
        WHEN fail.operation_id   IS NOT NULL THEN 'failure'
        ELSE 'pending'
    END AS status,
    r.amount_msat,
    succ.preimage
FROM receive r
LEFT JOIN receive_success succ
       ON succ.federation_id = r.federation_id AND succ.operation_id = r.operation_id
LEFT JOIN receive_failure fail
       ON fail.federation_id = r.federation_id AND fail.operation_id = r.operation_id
LEFT JOIN receive_refund  refund
       ON refund.federation_id = r.federation_id AND refund.operation_id = r.operation_id;
"#;

/// Spawn the per-client trailer: drain the event log forward in chunks and
/// mirror each gw event into the SQLite analytics DB. Blocks on
/// `event_notify` only when caught up with the head.
pub fn spawn_trailer(
    task_group: &TaskGroup,
    client: Arc<Client>,
    federation_id: FederationId,
    state: Analytics,
) {
    task_group.spawn_cancellable(
        "gw-analytics-trailer",
        trailer(client, federation_id, state),
    );
}

async fn trailer(client: Arc<Client>, federation_id: FederationId, state: Analytics) {
    let mut cursor = EventLogId::default();
    let notify = client.event_notify();
    let fed_id_hex = federation_id.to_string();

    loop {
        // Register interest in the next commit BEFORE reading, so we don't
        // miss a commit that lands between the read and `.await`.
        let notified = notify.notified();

        let chunk = client.get_event_log(Some(cursor), CHUNK_SIZE).await;
        if let Some((last_id, _)) = chunk.last() {
            cursor = last_id.saturating_add(1);
            let entries: Vec<EventLogEntry> = chunk.iter().map(|(_, e)| e.clone()).collect();
            let fed = fed_id_hex.clone();
            let state = state.clone();
            // rusqlite is sync — hop off the tokio runtime's thread pool for
            // the insert batch so we don't block other async work.
            if let Err(e) =
                tokio::task::spawn_blocking(move || insert_batch(&state, &fed, &entries))
                    .await
                    .expect("spawn_blocking join")
            {
                tracing::error!(
                    target: picomint_logging::LOG_GATEWAY,
                    %federation_id, error = %e,
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

fn insert_batch(state: &Analytics, fed_id: &str, entries: &[EventLogEntry]) -> anyhow::Result<()> {
    let mut guard = state.conn.blocking_lock();
    let tx = guard.transaction()?;
    for entry in entries {
        let op_id = entry.operation_id.to_string();
        let ts = entry.ts_usecs as i64;
        if let Some(e) = entry.to_event::<SendEvent>() {
            tx.execute(
                "INSERT OR IGNORE INTO send \
                 (federation_id, operation_id, ts, outpoint, amount_msat, ln_fee_msat, fee_msat) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    fed_id,
                    op_id,
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
                 (federation_id, operation_id, ts, preimage, txid, ln_fee_msat) \
                 VALUES (?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    fed_id,
                    op_id,
                    ts,
                    hex::encode(e.preimage),
                    e.txid.to_string(),
                    e.ln_fee.msats as i64,
                ],
            )?;
        } else if let Some(e) = entry.to_event::<SendCancelEvent>() {
            tx.execute(
                "INSERT OR IGNORE INTO send_cancel \
                 (federation_id, operation_id, ts, signature) VALUES (?, ?, ?, ?)",
                rusqlite::params![fed_id, op_id, ts, e.signature.to_string()],
            )?;
        } else if let Some(e) = entry.to_event::<ReceiveEvent>() {
            tx.execute(
                "INSERT OR IGNORE INTO receive \
                 (federation_id, operation_id, ts, txid, amount_msat, fee_msat) \
                 VALUES (?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    fed_id,
                    op_id,
                    ts,
                    e.txid.to_string(),
                    e.amount.msats as i64,
                    e.fee.msats as i64,
                ],
            )?;
        } else if let Some(e) = entry.to_event::<ReceiveSuccessEvent>() {
            tx.execute(
                "INSERT OR IGNORE INTO receive_success \
                 (federation_id, operation_id, ts, preimage) VALUES (?, ?, ?, ?)",
                rusqlite::params![fed_id, op_id, ts, hex::encode(e.preimage)],
            )?;
        } else if entry.to_event::<ReceiveFailureEvent>().is_some() {
            tx.execute(
                "INSERT OR IGNORE INTO receive_failure \
                 (federation_id, operation_id, ts) VALUES (?, ?, ?)",
                rusqlite::params![fed_id, op_id, ts],
            )?;
        } else if let Some(e) = entry.to_event::<ReceiveRefundEvent>() {
            tx.execute(
                "INSERT OR IGNORE INTO receive_refund \
                 (federation_id, operation_id, ts, txid) VALUES (?, ?, ?, ?)",
                rusqlite::params![fed_id, op_id, ts, e.txid.to_string()],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}
