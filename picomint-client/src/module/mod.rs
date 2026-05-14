use std::sync::Arc;

use crate::api::FederationApi;
use futures::StreamExt as _;
use futures::stream::BoxStream;
use picomint_core::TransactionId;
use picomint_core::config::ConsensusConfig;
use picomint_core::config::FederationId;
use picomint_core::core::OperationId;
use picomint_eventlog::{Event, EventLogEntry, EventLogId, EventLogger};
use picomint_redb::{Database, WriteTx};
use tokio::sync::Notify;

use crate::{TxAcceptEvent, TxRejectEvent};

/// Per-module bundle of API handles, the shared client db, and federation
/// config. Each module is constructed with one of these.
#[derive(Clone)]
pub struct ClientContext {
    api: FederationApi,
    db: Database,
    logger: EventLogger,
    config: ConsensusConfig,
}

impl ClientContext {
    pub fn new(
        api: FederationApi,
        db: Database,
        logger: EventLogger,
        config: ConsensusConfig,
    ) -> Self {
        Self {
            api,
            db,
            logger,
            config,
        }
    }

    pub fn network(&self) -> bitcoin::Network {
        self.config.network
    }

    /// Federation API handle. Typed wire methods are built with
    /// `Method::<Module>(<ModuleMethod>::...)` — there is no module-scope
    /// plumbing to attach.
    pub fn api(&self) -> FederationApi {
        self.api.clone()
    }

    pub fn db(&self) -> &Database {
        &self.db
    }

    pub async fn await_tx_accepted(
        &self,
        operation: OperationId,
        query_txid: TransactionId,
    ) -> Result<(), String> {
        let mut stream = self.subscribe_operation_events(operation);
        while let Some(entry) = stream.next().await {
            if let Some(ev) = entry.to_event::<TxAcceptEvent>()
                && ev.txid == query_txid
            {
                return Ok(());
            }
            if let Some(ev) = entry.to_event::<TxRejectEvent>()
                && ev.txid == query_txid
            {
                return Err(ev.error);
            }
        }
        unreachable!("subscribe_operation_events only ends at client shutdown")
    }

    pub fn get_config(&self) -> &ConsensusConfig {
        &self.config
    }

    pub fn federation(&self) -> FederationId {
        self.config.calculate_federation_id()
    }

    /// Shared [`Notify`] that fires on every commit touching the event log.
    pub fn event_notify(&self) -> Arc<Notify> {
        self.logger.event_notify(&self.db)
    }

    /// Read a batch of persisted event log entries starting at `pos`.
    pub async fn get_event_log(
        &self,
        pos: EventLogId,
        limit: u64,
    ) -> Vec<(EventLogId, EventLogEntry)> {
        self.logger.get_event_log(&self.db, pos, limit)
    }

    /// Stream every event belonging to `operation`, starting from the
    /// beginning of the log (existing events first, then live ones).
    pub fn subscribe_operation_events(
        &self,
        operation: OperationId,
    ) -> BoxStream<'static, EventLogEntry> {
        Box::pin(self.logger.subscribe_operation_events(
            self.db.clone(),
            self.event_notify(),
            operation,
        ))
    }

    pub fn log_event<E>(&self, dbtx: &WriteTx, operation: OperationId, event: E)
    where
        E: Event + Send,
    {
        self.logger
            .log_event(dbtx, self.federation(), operation, event);
    }

    pub fn logger(&self) -> &EventLogger {
        &self.logger
    }
}
