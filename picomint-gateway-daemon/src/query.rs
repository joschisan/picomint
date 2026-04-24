//! In-memory SQL query surface over the gateway's gw-module event log.
//!
//! Per-federation tail tasks mirror gw events into Arrow `RecordBatch`es, one
//! `Vec<RecordBatch>` per event kind. The `/query` handler snapshots this
//! state, builds a fresh `SessionContext` with a `payments` view on top, and
//! runs the user's SQL.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use arrow_json::ArrayWriter;
use datafusion::arrow::array::{
    ArrayRef, RecordBatch, StringBuilder, TimestampMicrosecondBuilder, UInt64Builder,
};
use datafusion::arrow::datatypes::{DataType, Field, Schema, SchemaRef, TimeUnit};
use datafusion::datasource::MemTable;
use datafusion::prelude::SessionContext;
use picomint_client::Client;
use picomint_client::gw::events::{
    ReceiveEvent, ReceiveFailureEvent, ReceiveRefundEvent, ReceiveSuccessEvent, SendCancelEvent,
    SendEvent, SendSuccessEvent,
};
use picomint_core::config::FederationId;
use picomint_core::core::OperationId;
use picomint_core::ln::LightningInvoice;
use picomint_core::secp256k1::schnorr::Signature;
use picomint_core::task::TaskGroup;
use picomint_core::{Amount, OutPoint, TransactionId};
use picomint_eventlog::{EventLogEntry, EventLogId};
use tokio::sync::RwLock;

const CHUNK_SIZE: u64 = 10_000;

/// All gw-module analytics tables — table name matches the event kind string.
const TABLES: &[&str] = &[
    "send",
    "send_success",
    "send_cancel",
    "receive",
    "receive_success",
    "receive_failure",
    "receive_refund",
];

fn common_fields() -> Vec<Field> {
    vec![
        Field::new("federation_id", DataType::Utf8, false),
        Field::new("operation_id", DataType::Utf8, false),
        Field::new(
            "ts",
            DataType::Timestamp(TimeUnit::Microsecond, None),
            false,
        ),
    ]
}

fn schema_for(table: &str) -> SchemaRef {
    let mut fields = common_fields();
    match table {
        "send" => {
            fields.push(Field::new("outpoint", DataType::Utf8, false));
            fields.push(Field::new("invoice", DataType::Utf8, false));
            fields.push(Field::new("amount_msat", DataType::UInt64, true));
        }
        "send_success" => {
            fields.push(Field::new("preimage", DataType::Utf8, false));
            fields.push(Field::new("txid", DataType::Utf8, false));
        }
        "send_cancel" => {
            fields.push(Field::new("signature", DataType::Utf8, false));
        }
        "receive" => {
            fields.push(Field::new("txid", DataType::Utf8, false));
            fields.push(Field::new("amount_msat", DataType::UInt64, false));
        }
        "receive_success" => {
            fields.push(Field::new("preimage", DataType::Utf8, false));
        }
        "receive_failure" => {}
        "receive_refund" => {
            fields.push(Field::new("txid", DataType::Utf8, false));
        }
        _ => unreachable!("unknown gw table: {table}"),
    }
    Arc::new(Schema::new(fields))
}

#[derive(Clone, Default)]
pub struct QueryState {
    inner: Arc<RwLock<BTreeMap<&'static str, Vec<RecordBatch>>>>,
}

impl QueryState {
    pub fn new() -> Self {
        Self::default()
    }

    async fn append(&self, table: &'static str, batch: RecordBatch) {
        self.inner
            .write()
            .await
            .entry(table)
            .or_default()
            .push(batch);
    }

    async fn snapshot(&self) -> BTreeMap<&'static str, Vec<RecordBatch>> {
        self.inner.read().await.clone()
    }
}

/// Spawn the per-client tailer: drain the event log forward in 10k-row
/// chunks, blocking on `event_notify` only when caught up with the head.
pub fn spawn_tail(
    task_group: &TaskGroup,
    client: Arc<Client>,
    federation_id: FederationId,
    state: QueryState,
) {
    task_group.spawn_cancellable("gw-analytics-tail", tail(client, federation_id, state));
}

async fn tail(client: Arc<Client>, federation_id: FederationId, state: QueryState) {
    let mut cursor = EventLogId::default();
    let notify = client.event_notify();

    loop {
        // Register interest in the next commit BEFORE reading, so we don't
        // miss a commit that lands between the read and `.await`.
        let notified = notify.notified();

        let chunk = client.get_event_log(Some(cursor), CHUNK_SIZE).await;
        if let Some((last_id, _)) = chunk.last() {
            cursor = last_id.saturating_add(1);
            let entries: Vec<&EventLogEntry> = chunk.iter().map(|(_, e)| e).collect();
            for (table, batch) in build_batches(federation_id, &entries) {
                state.append(table, batch).await;
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

/// Every gw-module event we care about, paired with its common fields
/// (`operation_id`, `ts_usecs`). `parse_gw_row` below walks each entry and
/// tries each typed `to_event::<E>()` in sequence — which already checks
/// both `EventSource::Gw` and the kind string — so mint/wallet/ln events
/// that share kind names ("send", "receive") drop through as `None`.
enum GwRow {
    Send(Common, SendEvent),
    SendSuccess(Common, SendSuccessEvent),
    SendCancel(Common, SendCancelEvent),
    Receive(Common, ReceiveEvent),
    ReceiveSuccess(Common, ReceiveSuccessEvent),
    ReceiveFailure(Common),
    ReceiveRefund(Common, ReceiveRefundEvent),
}

#[derive(Copy, Clone)]
struct Common {
    operation_id: OperationId,
    ts_usecs: u64,
}

fn parse_gw_row(entry: &EventLogEntry) -> Option<GwRow> {
    let common = Common {
        operation_id: entry.operation_id,
        ts_usecs: entry.ts_usecs,
    };
    if let Some(e) = entry.to_event::<SendEvent>() {
        return Some(GwRow::Send(common, e));
    }
    if let Some(e) = entry.to_event::<SendSuccessEvent>() {
        return Some(GwRow::SendSuccess(common, e));
    }
    if let Some(e) = entry.to_event::<SendCancelEvent>() {
        return Some(GwRow::SendCancel(common, e));
    }
    if let Some(e) = entry.to_event::<ReceiveEvent>() {
        return Some(GwRow::Receive(common, e));
    }
    if let Some(e) = entry.to_event::<ReceiveSuccessEvent>() {
        return Some(GwRow::ReceiveSuccess(common, e));
    }
    if entry.to_event::<ReceiveFailureEvent>().is_some() {
        return Some(GwRow::ReceiveFailure(common));
    }
    if let Some(e) = entry.to_event::<ReceiveRefundEvent>() {
        return Some(GwRow::ReceiveRefund(common, e));
    }
    None
}

/// Sort `entries` into per-kind buckets of typed events and emit one
/// `RecordBatch` per non-empty bucket.
fn build_batches(
    federation_id: FederationId,
    entries: &[&EventLogEntry],
) -> Vec<(&'static str, RecordBatch)> {
    let mut sends = Vec::new();
    let mut send_successes = Vec::new();
    let mut send_cancels = Vec::new();
    let mut receives = Vec::new();
    let mut receive_successes = Vec::new();
    let mut receive_failures = Vec::new();
    let mut receive_refunds = Vec::new();

    for entry in entries {
        match parse_gw_row(entry) {
            Some(GwRow::Send(c, e)) => sends.push((c, e)),
            Some(GwRow::SendSuccess(c, e)) => send_successes.push((c, e)),
            Some(GwRow::SendCancel(c, e)) => send_cancels.push((c, e)),
            Some(GwRow::Receive(c, e)) => receives.push((c, e)),
            Some(GwRow::ReceiveSuccess(c, e)) => receive_successes.push((c, e)),
            Some(GwRow::ReceiveFailure(c)) => receive_failures.push(c),
            Some(GwRow::ReceiveRefund(c, e)) => receive_refunds.push((c, e)),
            None => {}
        }
    }

    let mut out = Vec::new();
    if !sends.is_empty() {
        out.push(("send", build_gw_send(federation_id, &sends)));
    }
    if !send_successes.is_empty() {
        out.push((
            "send_success",
            build_gw_send_success(federation_id, &send_successes),
        ));
    }
    if !send_cancels.is_empty() {
        out.push((
            "send_cancel",
            build_gw_send_cancel(federation_id, &send_cancels),
        ));
    }
    if !receives.is_empty() {
        out.push(("receive", build_gw_receive(federation_id, &receives)));
    }
    if !receive_successes.is_empty() {
        out.push((
            "receive_success",
            build_gw_receive_success(federation_id, &receive_successes),
        ));
    }
    if !receive_failures.is_empty() {
        out.push((
            "receive_failure",
            build_common_only(federation_id, "receive_failure", &receive_failures),
        ));
    }
    if !receive_refunds.is_empty() {
        out.push((
            "receive_refund",
            build_gw_receive_refund(federation_id, &receive_refunds),
        ));
    }
    out
}

/// Build the three common columns (federation_id, operation_id, ts) shared
/// by every gw table. Returns the builders finalized into arrays.
fn common_columns(
    federation_id: FederationId,
    rows: impl ExactSizeIterator<Item = Common>,
) -> Vec<ArrayRef> {
    let fed_str = federation_id.to_string();
    let n = rows.len();

    let mut fed_b = StringBuilder::with_capacity(n, n * fed_str.len());
    let mut op_b = StringBuilder::with_capacity(n, n * 64);
    let mut ts_b = TimestampMicrosecondBuilder::with_capacity(n);

    for row in rows {
        fed_b.append_value(&fed_str);
        op_b.append_value(row.operation_id.0.to_string());
        ts_b.append_value(i64::try_from(row.ts_usecs).unwrap_or(i64::MAX));
    }

    vec![
        Arc::new(fed_b.finish()),
        Arc::new(op_b.finish()),
        Arc::new(ts_b.finish()),
    ]
}

/// For payload-less events (`gw_receive_failure`, `gw_complete`): the table
/// is just the three common columns.
fn build_common_only(
    federation_id: FederationId,
    table: &'static str,
    rows: &[Common],
) -> RecordBatch {
    let cols = common_columns(federation_id, rows.iter().copied());
    RecordBatch::try_new(schema_for(table), cols).expect("schema matches columns")
}

fn build_gw_send(federation_id: FederationId, rows: &[(Common, SendEvent)]) -> RecordBatch {
    let n = rows.len();
    let mut outpoint_b = StringBuilder::with_capacity(n, n * 72);
    let mut invoice_b = StringBuilder::with_capacity(n, n * 256);
    let mut amount_b = UInt64Builder::with_capacity(n);

    for (_, ev) in rows {
        outpoint_b.append_value(format_outpoint(&ev.outpoint));
        let LightningInvoice::Bolt11(invoice) = &ev.invoice;
        invoice_b.append_value(invoice.to_string());
        match invoice.amount_milli_satoshis() {
            Some(msat) => amount_b.append_value(msat),
            None => amount_b.append_null(),
        }
    }

    let mut cols = common_columns(federation_id, rows.iter().map(|(c, _)| *c));
    cols.push(Arc::new(outpoint_b.finish()));
    cols.push(Arc::new(invoice_b.finish()));
    cols.push(Arc::new(amount_b.finish()));
    RecordBatch::try_new(schema_for("send"), cols).expect("schema matches columns")
}

fn build_gw_send_success(
    federation_id: FederationId,
    rows: &[(Common, SendSuccessEvent)],
) -> RecordBatch {
    let n = rows.len();
    let mut preimage_b = StringBuilder::with_capacity(n, n * 64);
    let mut txid_b = StringBuilder::with_capacity(n, n * 64);

    for (_, ev) in rows {
        preimage_b.append_value(hex::encode(ev.preimage));
        txid_b.append_value(format_txid(&ev.txid));
    }

    let mut cols = common_columns(federation_id, rows.iter().map(|(c, _)| *c));
    cols.push(Arc::new(preimage_b.finish()));
    cols.push(Arc::new(txid_b.finish()));
    RecordBatch::try_new(schema_for("send_success"), cols).expect("schema matches columns")
}

fn build_gw_send_cancel(
    federation_id: FederationId,
    rows: &[(Common, SendCancelEvent)],
) -> RecordBatch {
    let n = rows.len();
    let mut sig_b = StringBuilder::with_capacity(n, n * 128);

    for (_, ev) in rows {
        sig_b.append_value(format_signature(&ev.signature));
    }

    let mut cols = common_columns(federation_id, rows.iter().map(|(c, _)| *c));
    cols.push(Arc::new(sig_b.finish()));
    RecordBatch::try_new(schema_for("send_cancel"), cols).expect("schema matches columns")
}

fn build_gw_receive(federation_id: FederationId, rows: &[(Common, ReceiveEvent)]) -> RecordBatch {
    let n = rows.len();
    let mut txid_b = StringBuilder::with_capacity(n, n * 64);
    let mut amount_b = UInt64Builder::with_capacity(n);

    for (_, ev) in rows {
        txid_b.append_value(format_txid(&ev.txid));
        amount_b.append_value(format_amount(ev.amount));
    }

    let mut cols = common_columns(federation_id, rows.iter().map(|(c, _)| *c));
    cols.push(Arc::new(txid_b.finish()));
    cols.push(Arc::new(amount_b.finish()));
    RecordBatch::try_new(schema_for("receive"), cols).expect("schema matches columns")
}

fn build_gw_receive_success(
    federation_id: FederationId,
    rows: &[(Common, ReceiveSuccessEvent)],
) -> RecordBatch {
    let n = rows.len();
    let mut preimage_b = StringBuilder::with_capacity(n, n * 64);

    for (_, ev) in rows {
        preimage_b.append_value(hex::encode(ev.preimage));
    }

    let mut cols = common_columns(federation_id, rows.iter().map(|(c, _)| *c));
    cols.push(Arc::new(preimage_b.finish()));
    RecordBatch::try_new(schema_for("receive_success"), cols).expect("schema matches columns")
}

fn build_gw_receive_refund(
    federation_id: FederationId,
    rows: &[(Common, ReceiveRefundEvent)],
) -> RecordBatch {
    let n = rows.len();
    let mut txid_b = StringBuilder::with_capacity(n, n * 64);

    for (_, ev) in rows {
        txid_b.append_value(format_txid(&ev.txid));
    }

    let mut cols = common_columns(federation_id, rows.iter().map(|(c, _)| *c));
    cols.push(Arc::new(txid_b.finish()));
    RecordBatch::try_new(schema_for("receive_refund"), cols).expect("schema matches columns")
}

fn format_outpoint(o: &OutPoint) -> String {
    format!("{}:{}", o.txid, o.out_idx)
}

fn format_txid(t: &TransactionId) -> String {
    t.to_string()
}

fn format_signature(s: &Signature) -> String {
    s.to_string()
}

fn format_amount(a: Amount) -> u64 {
    a.msats
}

/// SQL stitching the event tables into a single wide `payments` row per operation.
/// Dashed table names need double-quoting per ANSI SQL.
const PAYMENTS_VIEW_SQL: &str = r#"
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
LEFT JOIN send_success succ ON s.operation_id = succ.operation_id
LEFT JOIN send_cancel  canc ON s.operation_id = canc.operation_id
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
    CAST(r.amount_msat AS BIGINT UNSIGNED) AS amount_msat,
    succ.preimage
FROM receive r
LEFT JOIN receive_success succ   ON r.operation_id = succ.operation_id
LEFT JOIN receive_failure fail   ON r.operation_id = fail.operation_id
LEFT JOIN receive_refund  refund ON r.operation_id = refund.operation_id
"#;

/// Build a fresh `SessionContext` from a snapshot of `state` and register the
/// `payments` view. New context per query — the registered `MemTable`s wrap
/// `Arc`'d batches so this is O(1) clone, not a data copy.
pub async fn build_session(state: &QueryState) -> Result<SessionContext> {
    let snapshot = state.snapshot().await;
    let ctx = SessionContext::new();

    for table in TABLES {
        let schema = schema_for(table);
        let batches = snapshot.get(table).cloned().unwrap_or_default();
        let mem = MemTable::try_new(schema, vec![batches])?;
        ctx.register_table(*table, Arc::new(mem))?;
    }

    ctx.sql(PAYMENTS_VIEW_SQL)
        .await
        .context("failed to prepare payments view")?
        .collect()
        .await
        .context("failed to register payments view")?;

    Ok(ctx)
}

/// Run `sql` against the current analytics snapshot, return rows as a JSON array.
///
/// Each row is a JSON object keyed by column name. Column order is preserved
/// by the `preserve_order` feature on `serde_json` in the workspace.
pub async fn run_query(state: &QueryState, sql: &str) -> Result<serde_json::Value> {
    let ctx = build_session(state).await?;
    let df = ctx.sql(sql).await.context("SQL parse/plan failed")?;
    let batches = df.collect().await.context("query execution failed")?;

    let mut buf = Vec::new();
    let mut writer = ArrayWriter::new(&mut buf);
    for batch in &batches {
        writer.write(batch)?;
    }
    writer.finish()?;
    drop(writer);

    if buf.is_empty() {
        return Ok(serde_json::Value::Array(Vec::new()));
    }

    serde_json::from_slice(&buf).context("failed to parse arrow-json output")
}
