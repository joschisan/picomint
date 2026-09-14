//! Event log
//!
//! Single, ordered, append-only log of what consensus did: every input
//! and output of an accepted transaction, one event per module variant,
//! a deposit claimed with its value, a contract funded and settled, a
//! block tracked, the consensus height or feerate moving. Every event is written inside the
//! write transaction of the item that caused it, by deterministic
//! processing of the ordered stream, so the log is byte-identical on
//! every node and a restored node rewrites it from session zero as it
//! replays.
//!
//! There is no wallclock: a replay would stamp the whole history with the
//! replay time. The consensus height is itself an event, and everything
//! between two height events happened at the first one's height.
//!
//! The mirror in `consensus/analytics.rs` translates the log into SQLite.

use picomint_core::sql::SqlRow;
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::{DbRead, WriteTx, table};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// An event type. The kind is the log-wide name of the event: core
/// events are unprefixed (`height`) and module events carry their module
/// (`ecash-input`), the same rule the client's events follow; the mirror
/// names the table after it. The payload is the struct as JSON, like the
/// client's.
pub trait Event: Serialize + DeserializeOwned + SqlRow {
    const KIND: &'static str;
}

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct EventLogEntry {
    pub kind: String,
    pub payload: Vec<u8>,
}

impl EventLogEntry {
    pub fn to_event<E: Event>(&self) -> Option<E> {
        (self.kind == E::KIND)
            .then(|| serde_json::from_slice(&self.payload).ok())
            .flatten()
    }
}

table!(
    EventLogTable,
    u64 => EventLogEntry,
    "event-log",
);

/// Append an event under the next id. Ids are allocated inline under the
/// database's single-writer serialization, so they are dense.
pub fn log_event<E: Event>(dbtx: &WriteTx, event: &E) {
    let id = dbtx.iter_rev(&EventLogTable, |r| r.next().map_or(0, |entry| entry.0 + 1));

    let entry = EventLogEntry {
        kind: E::KIND.to_string(),
        payload: serde_json::to_vec(event).expect("Serialization can't fail"),
    };

    dbtx.insert_new(&EventLogTable, &id, &entry);
}
