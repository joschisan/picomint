//! The client's analytics registry: one table per event, named after
//! its source and kind — `gateway_send`, `core_tx_accept` — whose
//! columns are the entry's common fields followed by the event's own.
//! `picomint-analytics` mirrors the event log through [`schema`],
//! [`rows`] and [`reader`].

use std::sync::Arc;

use picomint_core::sql::{Row, SqlRow, SqlValue, create_table};
use tracing::debug;

use crate::eventlog::{Event, EventLogEntry, EventLogId, EventSource};
use crate::{Client, ecash, gateway, lightning, onchain};

/// Every event the mirror holds. An event type missing here is logged
/// at debug level and skipped, so adding an event means adding it here.
macro_rules! events {
    ($($event:path),* $(,)?) => {
        /// The DDL for every registered event's table.
        pub fn schema() -> String {
            let mut sql = String::new();
            $(sql.push_str(&create_table::<$event>(&table_name::<$event>(), &COMMON_COLUMNS, &INDEXES));)*
            sql
        }

        /// The row an entry lands as, empty for an unregistered event.
        pub fn rows(entry: &(EventLogId, EventLogEntry)) -> Vec<Row> {
            $(if let Some(event) = entry.1.to_event::<$event>() {
                return vec![row::<$event>(entry.0, &entry.1, &event)];
            })*

            debug!(kind = %entry.1.kind, source = ?entry.1.source, "event not registered for analytics");

            Vec::new()
        }
    };
}

events! {
    crate::TxCreateEvent,
    crate::TxAcceptEvent,
    crate::TxRejectEvent,
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

/// The entry fields every table starts with, ahead of the event's own.
const COMMON_COLUMNS: [(&str, &str); 5] = [
    ("id", "INTEGER PRIMARY KEY"),
    ("ts", "INTEGER NOT NULL"),
    ("mint", "TEXT NOT NULL"),
    ("account", "TEXT NOT NULL"),
    ("operation", "TEXT NOT NULL"),
];

const INDEXES: [&str; 2] = ["operation", "ts"];

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

fn row<E: Event + SqlRow>(id: EventLogId, entry: &EventLogEntry, event: &E) -> Row {
    let mut values = vec![
        SqlValue::Integer(id.0.cast_signed()),
        SqlValue::Integer(entry.timestamp.cast_signed()),
        SqlValue::Text(entry.mint.to_string()),
        SqlValue::Text(format!("{:?}", entry.account)),
        SqlValue::Text(entry.operation.to_string()),
    ];

    values.extend(event.values());

    Row {
        table: table_name::<E>(),
        values,
    }
}

/// Reads the client's event log forward: each call returns up to `limit`
/// entries past the ones returned before.
pub fn reader(client: Arc<Client>) -> impl FnMut(u64) -> Vec<(EventLogId, EventLogEntry)> {
    let mut cursor = EventLogId::default();

    move |limit| {
        let chunk = client.get_event_log(cursor, limit);

        if let Some(entry) = chunk.last() {
            cursor = entry.0.saturating_add(1);
        }

        chunk
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every registered event's table installs, which is what catches a
    /// field type without a column mapping or two events sharing a name.
    #[test]
    fn schema_installs() {
        let sql = schema();

        println!("{sql}");

        rusqlite::Connection::open_in_memory()
            .expect("in-memory sqlite")
            .execute_batch(&sql)
            .expect("schema installs");
    }
}
