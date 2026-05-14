use std::sync::Arc;
use std::sync::atomic::AtomicU8;

use anyhow::bail;
use futures::StreamExt as _;
use picomint_redb::{Database, table};
use tokio::try_join;
use tracing::info;

use super::{EventKind, EventLogEntry, EventLogId, EventLogger, EventSource};

table!(TestEventLogTable, EventLogId => EventLogEntry, "test-event-log");
table!(
    TestEventLogByOperationTable,
    (picomint_core::core::OperationId, EventLogId) => EventLogEntry,
    "test-event-log-by-operation",
);

#[test_log::test(tokio::test)]
async fn sanity_subscribe_operation_events() {
    let db = Database::open_in_memory();
    let logger = EventLogger::new(TestEventLogTable, TestEventLogByOperationTable);
    let event_notify = logger.event_notify(&db);

    let operation = picomint_core::core::OperationId::new_random();
    let counter = Arc::new(AtomicU8::new(0));

    let _ = try_join!(
        {
            let counter = counter.clone();
            let db = db.clone();
            let event_notify = event_notify.clone();
            let logger = logger.clone();
            async move {
                let mut stream =
                    Box::pin(logger.subscribe_operation_events(db, event_notify, operation));
                while let Some(entry) = stream.next().await {
                    info!("{entry:?}");
                    assert_eq!(
                        entry.kind,
                        EventKind::from(format!(
                            "{}",
                            counter.load(std::sync::atomic::Ordering::Relaxed)
                        ))
                    );
                    if counter.load(std::sync::atomic::Ordering::Relaxed) == 4 {
                        bail!("Time to wrap up");
                    }
                    counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                Ok(())
            }
        },
        async {
            let federation = picomint_core::config::FederationId::dummy();
            for i in 0..=4 {
                let dbtx = db.begin_write();
                logger.log_event_raw(
                    &dbtx,
                    EventKind::from(format!("{i}")),
                    EventSource::Core,
                    federation,
                    operation,
                    vec![],
                );

                dbtx.commit();
            }

            Ok(())
        }
    );
}
