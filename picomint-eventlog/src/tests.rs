use std::sync::Arc;
use std::sync::atomic::AtomicU8;

use anyhow::bail;
use futures::StreamExt as _;
use picomint_sqlite::Database;
use tokio::try_join;
use tracing::info;

use super::{EventKind, EventSource, log_event_raw, subscribe_operation_events};

#[test_log::test(tokio::test)]
async fn sanity_subscribe_operation_events() {
    let dir = tempfile::TempDir::new().expect("failed to create temp dir");

    let db = Database::open(dir.path().join("test.sqlite")).expect("sqlite open failed");

    let operation = picomint_core::core::OperationId::new_random();
    let counter = Arc::new(AtomicU8::new(0));

    let _ = try_join!(
        {
            let counter = counter.clone();
            let db = db.clone();
            async move {
                let mut stream = Box::pin(subscribe_operation_events(db, operation));
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
                log_event_raw(
                    &dbtx,
                    EventKind::from(format!("{i}")),
                    EventSource::Core,
                    federation,
                    picomint_core::core::Account::PRIMARY,
                    operation,
                    vec![],
                );

                dbtx.commit();
            }

            Ok(())
        }
    );
}
