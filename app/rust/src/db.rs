//! Pico's app-level redb tables. Per-federation client state lives in
//! `"{federation}/..."`-prefixed tables owned entirely by
//! `picomint_client::Client`; the daemon-wide event log is backed by the
//! two tables declared here and surfaced via `picomint_eventlog::EventLogger`.

use picomint_core::config::{ConsensusConfig, FederationId};
use picomint_core::core::OperationId;
use picomint_eventlog::{EventLogEntry, EventLogId};
use picomint_redb::table;

table!(
    RootEntropy,
    () => Vec<u8>,
    "root-entropy",
);

table!(
    ClientConfig,
    FederationId => ConsensusConfig,
    "client-config",
);

table!(
    SelectedCurrency,
    () => String,
    "selected-currency",
);

// Exchange rate snapshotted against an operation when its trigger event is
// first observed live, so history can show the fiat value as of the time of
// the payment. `(currency_code, btc_price)`; absent for operations that
// predate the feature or landed with no fresh rate cached.
table!(
    OperationFiat,
    OperationId => (String, f64),
    "operation-fiat",
);

table!(
    CONTACT,
    String => String,
    "contact",
);

// Daemon-wide event log. The two tables are owned here (not by
// `picomint-eventlog`) so the app controls the on-disk schema; an
// `EventLogger` is constructed over them at factory startup.
table!(
    EventLog,
    EventLogId => EventLogEntry,
    "event-log",
);

table!(
    EventLogByOperation,
    (OperationId, EventLogId) => EventLogEntry,
    "operation-event-log",
);
