//! The client's analytics registry: one table per event, named after
//! its kind — `gateway_send`, `tx_accept` — whose columns are the entry's
//! common fields followed by the event's own. `picomint-analytics`
//! mirrors the event log through [`schema`], [`rows`] and [`reader`];
//! [`tables`] is what `query --help` prints.

use std::sync::Arc;

use picomint_core::sql::{Row, SqlRow, SqlValue, create_table, table_doc};
use tracing::debug;

use crate::eventlog::{Event, EventLogEntry, EventLogId};
use crate::{Client, ecash, gateway, lightning, onchain};

/// Every event the mirror holds. An event type missing here is logged
/// at debug level and skipped, so adding an event means adding it here.
macro_rules! events {
    ($($event:path),* $(,)?) => {
        /// Every table with its columns explained: what the CLI's
        /// `query --help` prints so an agent knows the schema without a
        /// running daemon.
        pub fn tables() -> String {
            let mut text = COMMON_COLUMNS_DOC.to_string();
            $(text.push_str(&table_doc::<$event>(&table_name::<$event>()));)*
            text
        }

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

            debug!(kind = %entry.1.kind, "event not registered for analytics");

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

/// How `query --help` explains the common columns, ahead of the tables.
const COMMON_COLUMNS_DOC: &str = "\
Tables, one per event. Every table starts with the same columns:
  id INTEGER         The row's position in the event log
  ts INTEGER         When the event was logged, in ms since the unix epoch
  mint TEXT          The mint id, hex
  account TEXT       primary, secondary, tertiary, quaternary or quinary
  operation TEXT     The operation the event belongs to, hex; the events of one send or receive share it across tables
Every table is indexed on operation and ts. Amounts are integers, msat except in the onchain tables, which are sat; hashes, ids and keys are text. The table's own columns follow its name.
";

/// Table name for an event: its kind, spelled the way SQL likes it —
/// `gateway_send`, `tx_accept`.
pub fn table_name<E: Event>() -> String {
    E::KIND.to_string().replace('-', "_")
}

fn row<E: Event + SqlRow>(id: EventLogId, entry: &EventLogEntry, event: &E) -> Row {
    let mut values = vec![
        SqlValue::Integer(id.0.cast_signed()),
        SqlValue::Integer(entry.timestamp.cast_signed()),
        SqlValue::Text(entry.mint.to_string()),
        SqlValue::Text(entry.account.to_string().to_lowercase()),
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
