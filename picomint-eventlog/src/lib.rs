#![allow(clippy::needless_lifetimes)]

//! Event log
//!
//! Single, ordered, append-only log of all important events on a host.
//! Events that carry an `operation` are additionally duplicated into a
//! secondary table keyed by `(operation, event_log_id)` so a subscriber
//! can tail events for a specific operation cheaply via a stream API.
//!
//! The crate owns the two tables and exposes the log/subscribe operations
//! as free functions over the caller's [`Database`] / [`WriteTx`].
use std::borrow::Cow;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use derive_more::Display;
use futures::Stream;
use picomint_core::config::FederationId;
use picomint_core::core::{Account, OperationId};
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::{Database, DbRead, WriteTx, table};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    Encodable,
    Decodable,
)]
pub enum EventSource {
    Core,
    Mint,
    Wallet,
    Ln,
    Gw,
}

pub trait Event: serde::Serialize + serde::de::DeserializeOwned {
    const SOURCE: EventSource;
    const KIND: EventKind;
}

/// Ordered, contiguous ID space — easy for event log followers to track.
#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    Encodable,
    Decodable,
)]
pub struct EventLogId(pub u64);

impl EventLogId {
    pub const LOG_START: EventLogId = EventLogId(0);
    pub const LOG_END: EventLogId = EventLogId(u64::MAX);

    fn next(self) -> EventLogId {
        Self(self.0 + 1)
    }

    pub fn saturating_add(self, rhs: u64) -> EventLogId {
        Self(self.0.saturating_add(rhs))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Encodable, Decodable, Display)]
pub struct EventKind(Cow<'static, str>);

impl EventKind {
    pub const fn from_static(value: &'static str) -> Self {
        Self(Cow::Borrowed(value))
    }
}

impl<'s> From<&'s str> for EventKind {
    fn from(value: &'s str) -> Self {
        Self(Cow::Owned(value.to_owned()))
    }
}

impl From<String> for EventKind {
    fn from(value: String) -> Self {
        Self(Cow::Owned(value))
    }
}

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct EventLogEntry {
    pub kind: EventKind,

    /// Where the event came from. See [`EventSource`].
    pub source: EventSource,

    /// Federation this event belongs to. Every event is federation-scoped
    /// — there are no global events. For events that span two clients in
    /// the same daemon (e.g. a gateway-internal direct swap), each side
    /// emits its own entry tagged with its own federation; the shared
    /// `operation` lets a subscriber stitch them together.
    pub federation: FederationId,

    /// Account within that federation whose balance the event concerns.
    /// Accounts share one log, so this is what lets a subscriber attribute
    /// an entry to the balance it moved.
    pub account: Account,

    /// Operation this event belongs to. Used to index the event into the
    /// by-operation secondary table for op-scoped tailing.
    pub operation: OperationId,

    /// Timestamp in milliseconds after unix epoch.
    pub timestamp: u64,

    /// Event-kind specific payload, typically json-encoded.
    pub payload: Vec<u8>,
}

impl EventLogEntry {
    pub fn to_event<E: Event>(&self) -> Option<E> {
        (self.source == E::SOURCE && self.kind == E::KIND)
            .then(|| serde_json::from_slice(&self.payload).ok())
            .flatten()
    }
}

// ─── Tables + operations ─────────────────────────────────────────────────

table!(
    EventLogTable,
    EventLogId => EventLogEntry,
    "event-log",
);

table!(
    EventLogByOperationTable,
    (OperationId, EventLogId) => EventLogEntry,
    "operation-event-log",
);

/// Append an event to the two tables. IDs are allocated inline under the
/// database's single-writer serialization. The per-table [`Notify`] for the
/// main event-log table is woken automatically on commit.
pub fn log_event_raw(
    dbtx: &WriteTx,
    kind: EventKind,
    source: EventSource,
    federation: FederationId,
    account: Account,
    operation: OperationId,
    payload: Vec<u8>,
) {
    tracing::info!(
        kind = %kind,
        source = ?source,
        %federation,
        %account,
        operation = %operation,
        payload = %String::from_utf8_lossy(&payload),
        "event",
    );

    let id = next_event_log_id(dbtx);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("System time before Unix epoch")
        .as_millis() as u64;
    let entry = EventLogEntry {
        kind,
        source,
        federation,
        account,
        operation,
        timestamp,
        payload,
    };

    assert!(
        dbtx.insert(&EventLogTable, &id, &entry).is_none(),
        "Must never overwrite existing event"
    );

    assert!(
        dbtx.insert(&EventLogByOperationTable, &(operation, id), &entry)
            .is_none(),
        "Must never overwrite existing event"
    );
}

/// Typed convenience: encode an [`Event`] into the log.
pub fn log_event<E: Event>(
    dbtx: &WriteTx,
    federation: FederationId,
    account: Account,
    operation: OperationId,
    event: E,
) {
    log_event_raw(
        dbtx,
        E::KIND,
        E::SOURCE,
        federation,
        account,
        operation,
        serde_json::to_vec(&event).expect("Serialization can't fail"),
    );
}

fn next_event_log_id(dbtx: &WriteTx) -> EventLogId {
    dbtx.iter_rev(&EventLogTable, |it| {
        it.next().map(|entry| entry.0.next()).unwrap_or_default()
    })
}

/// [`Notify`] handle that fires on every commit touching the event log
/// table.
pub fn event_notify(db: &Database) -> Arc<Notify> {
    db.notify_for_table(&EventLogTable)
}

/// Read up to `limit` consecutive event-log entries starting at `pos`.
/// Trailers paging through the log in chunks call this in a loop,
/// advancing `pos` past the last returned id between calls. Pass
/// [`EventLogId::LOG_START`] to read from the head.
pub fn get_event_log(
    db: &Database,
    pos: EventLogId,
    limit: u64,
) -> Vec<(EventLogId, EventLogEntry)> {
    let end = pos.saturating_add(limit);
    db.begin_read()
        .range(&EventLogTable, pos..end, |it| it.collect())
}

/// One-shot snapshot of every event currently logged for `operation`,
/// in insertion order. See [`subscribe_operation_events`] for the
/// streaming variant that also yields events arriving after the call.
pub fn read_operation_events(db: &Database, operation: OperationId) -> Vec<EventLogEntry> {
    db.begin_read()
        .prefix(&EventLogByOperationTable, &operation, |it| {
            it.map(|entry| entry.1).collect()
        })
}

/// Stream every event belonging to `operation`, in insertion order.
///
/// Yields existing events first, then live ones. The cursor is kept
/// internally — callers never manage an `EventLogId`. The stream runs
/// forever; callers stop tailing by dropping it.
pub fn subscribe_operation_events(
    db: Database,
    operation: OperationId,
) -> impl Stream<Item = EventLogEntry> + 'static {
    let event_notify = event_notify(&db);
    async_stream::stream! {
        let mut next_id = EventLogId::LOG_START;
        loop {
            let notified = event_notify.notified();
            let batch: Vec<(EventLogId, EventLogEntry)> = db.begin_read().range(
                &EventLogByOperationTable,
                (operation, next_id)..(operation, EventLogId::LOG_END),
                |it| it.map(|entry| (entry.0.1, entry.1)).collect(),
            );
            for (id, entry) in batch {
                next_id = id.next();
                yield entry;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests;
