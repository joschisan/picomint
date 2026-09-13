//! The node's analytics registry: one table per event, named after its
//! kind — `height`, `ecash_input`, `onchain_tx` — whose columns are the
//! log id followed by the event's own. `picomint-analytics` mirrors the
//! event log through [`schema`], [`rows`] and [`reader`]; [`tables`] is
//! what `query --help` prints.
//!
//! No wallclock column: the consensus height is the `height` table, and
//! a row's height is the last height event before its id. The mirror is
//! byte-identical on every node.

use picomint_core::sql::{Row, SqlValue, create_table, table_doc};
use picomint_redb::{Database, DbRead};
use tracing::debug;

use crate::consensus::eventlog::{Event, EventLogEntry, EventLogTable};
use crate::consensus::{ecash, events, lightning, onchain};

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
            $(sql.push_str(&create_table::<$event>(&table_name::<$event>(), &COMMON_COLUMNS, &[]));)*
            sql
        }

        /// The row an entry lands as, empty for an unregistered event.
        pub fn rows(entry: &(u64, EventLogEntry)) -> Vec<Row> {
            $(if let Some(event) = entry.1.to_event::<$event>() {
                return vec![row::<$event>(entry.0, &event)];
            })*

            debug!(kind = %entry.1.kind, "event not registered for analytics");

            Vec::new()
        }
    };
}

events! {
    events::HeightEvent,
    events::VersionEvent,
    events::SessionEvent,
    ecash::events::InputEvent,
    ecash::events::OutputEvent,
    onchain::events::InputEvent,
    onchain::events::OutputEvent,
    onchain::events::TxEvent,
    onchain::events::TxSignedEvent,
    onchain::events::TxConfirmedEvent,
    onchain::events::BlockEvent,
    onchain::events::TrackedOutputEvent,
    onchain::events::FeerateEvent,
    lightning::events::OutputOutgoingEvent,
    lightning::events::OutputIncomingEvent,
    lightning::events::InputOutgoingClaimEvent,
    lightning::events::InputOutgoingRefundEvent,
    lightning::events::InputOutgoingCancelEvent,
    lightning::events::InputIncomingClaimEvent,
    lightning::events::InputIncomingRefundEvent,
}

/// The log id, ahead of every table's own columns.
const COMMON_COLUMNS: [(&str, &str); 1] = [("id", "INTEGER PRIMARY KEY")];

/// How `query --help` explains the common column, ahead of the tables.
const COMMON_COLUMNS_DOC: &str = "\
Tables, one per consensus event. Every table starts with the same column:
  id INTEGER         The row's position in the event log, the order consensus processed it in
There is no wallclock: the consensus block height is the height table, and a row happened at the height of the last height row before it. Amounts are integers, msat except the _sat columns; hashes, ids and keys are text. The table's own columns follow its name.
";

/// Table name for an event: its kind, spelled the way SQL likes it —
/// `height`, `ecash_input`.
pub fn table_name<E: Event>() -> String {
    E::KIND.replace('-', "_")
}

fn row<E: Event>(id: u64, event: &E) -> Row {
    let mut values = vec![SqlValue::Integer(id.cast_signed())];

    values.extend(event.values());

    Row {
        table: table_name::<E>(),
        values,
    }
}

/// Reads the log forward: each call returns up to `limit` entries past
/// the ones returned before.
pub fn reader(db: Database) -> impl FnMut(u64) -> Vec<(u64, EventLogEntry)> {
    let mut cursor = 0u64;

    move |limit| {
        let chunk = db.begin_read().range(&EventLogTable, cursor.., |r| {
            r.take(limit as usize).collect::<Vec<_>>()
        });

        if let Some(last) = chunk.last() {
            cursor = last.0 + 1;
        }

        chunk
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every registered table installs, which is what catches a field
    /// type without a column mapping or two events sharing a table name.
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
