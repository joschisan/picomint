#![allow(clippy::needless_lifetimes)]

//! Client Event Log
//!
//! Single, ordered, append-only log of all important client-side events.
//! Events that carry an `operation_id` are additionally duplicated into a
//! secondary table keyed by `(operation_id, event_log_id)` so a subscriber
//! can tail events for a specific operation cheaply via a stream API.
use std::borrow::Cow;
use std::sync::Arc;

use derive_more::{Display, FromStr};
use futures::Stream;
use picomint_core::core::OperationId;
use picomint_core::time::duration_since_epoch;
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::NativeTableDef;
use picomint_redb::{Database, WriteTxRef};
use picomint_redb::{consensus_key, consensus_value};
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
    Display,
    FromStr,
)]
pub struct EventLogId(pub u64);

consensus_key!(EventLogId);

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

impl From<EventLogId> for u64 {
    fn from(value: EventLogId) -> Self {
        value.0
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

    /// Operation this event belongs to. Used to index the event into
    /// [`EVENT_LOG_BY_OPERATION`] for op-scoped tailing.
    pub operation_id: OperationId,

    /// Timestamp in microseconds after unix epoch.
    pub ts_usecs: u64,

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

consensus_value!(EventLogEntry);

pub const EVENT_LOG: NativeTableDef<EventLogId, EventLogEntry> = NativeTableDef::new("event-log");

pub const EVENT_LOG_BY_OPERATION: NativeTableDef<(OperationId, EventLogId), EventLogEntry> =
    NativeTableDef::new("event-log-by-operation");

/// Append an event to [`EVENT_LOG`] and [`EVENT_LOG_BY_OPERATION`]. IDs are
/// allocated inline under redb's single-writer serialization. The per-table
/// [`Notify`] for `EVENT_LOG` is woken automatically on commit by the redb
/// layer.
pub fn log_event_raw(
    dbtx: &WriteTxRef<'_>,
    kind: EventKind,
    source: EventSource,
    operation_id: OperationId,
    payload: Vec<u8>,
) {
    tracing::info!(
        kind = %kind,
        source = ?source,
        operation_id = %operation_id,
        payload = %String::from_utf8_lossy(&payload),
        "event",
    );

    let id = next_event_log_id(dbtx);
    let ts_usecs = u64::try_from(duration_since_epoch().as_micros()).unwrap_or(u64::MAX);
    let entry = EventLogEntry {
        kind,
        source,
        operation_id,
        ts_usecs,
        payload,
    };

    assert!(
        dbtx.insert(&EVENT_LOG, &id, &entry).is_none(),
        "Must never overwrite existing event"
    );

    assert!(
        dbtx.insert(&EVENT_LOG_BY_OPERATION, &(operation_id, id), &entry)
            .is_none(),
        "Must never overwrite existing event"
    );
}

/// Typed convenience: encode an [`Event`] into the log.
pub fn log_event<E: Event>(dbtx: &WriteTxRef<'_>, operation_id: OperationId, event: E) {
    log_event_raw(
        dbtx,
        E::KIND,
        E::SOURCE,
        operation_id,
        serde_json::to_vec(&event).expect("Serialization can't fail"),
    );
}

/// Next unused log id — one past the max existing id, or 0 if empty.
fn next_event_log_id(dbtx: &WriteTxRef<'_>) -> EventLogId {
    dbtx.iter(&EVENT_LOG, |it| {
        it.next_back().map(|(k, _)| k.next()).unwrap_or_default()
    })
}

/// One-shot snapshot of every event currently logged for `operation_id`, in
/// insertion order. See [`subscribe_operation_events`] for the streaming
/// variant that also yields events arriving after the call.
pub fn read_operation_events(db: &Database, operation_id: OperationId) -> Vec<EventLogEntry> {
    db.begin_read().range(
        &EVENT_LOG_BY_OPERATION,
        (operation_id, EventLogId::LOG_START)..(operation_id, EventLogId::LOG_END),
        |it| it.map(|(_, v)| v).collect(),
    )
}

/// Stream every event belonging to `operation_id`, in insertion order.
///
/// Yields existing events first, then live ones. The cursor is kept internally
/// — callers never manage an `EventLogId`. The stream runs forever; callers
/// stop tailing by dropping it.
pub fn subscribe_operation_events(
    db: Database,
    event_notify: Arc<Notify>,
    operation_id: OperationId,
) -> impl Stream<Item = EventLogEntry> {
    async_stream::stream! {
        let mut next_id = EventLogId::LOG_START;
        loop {
            let notified = event_notify.notified();
            let batch: Vec<(EventLogId, EventLogEntry)> = db.begin_read().range(
                &EVENT_LOG_BY_OPERATION,
                (operation_id, next_id)..(operation_id, EventLogId::LOG_END),
                |it| it.map(|((_, id), entry)| (id, entry)).collect(),
            );
            for (id, entry) in batch {
                next_id = id.next();
                yield entry;
            }
            notified.await;
        }
    }
}

/// Typed variant of [`subscribe_operation_events`] — filters by
/// `E::KIND`/`E::MODULE` and decodes each matching entry.
pub fn subscribe_operation_events_typed<E: Event + 'static>(
    db: Database,
    event_notify: Arc<Notify>,
    operation_id: OperationId,
) -> impl Stream<Item = E> {
    use futures::StreamExt as _;
    subscribe_operation_events(db, event_notify, operation_id)
        .filter_map(|entry| async move { entry.to_event::<E>() })
}

#[cfg(test)]
mod tests;
