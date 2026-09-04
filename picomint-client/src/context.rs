use crate::api::MintApi;
use crate::eventlog::{Event, EventLogEntry};
use crate::lightning::Gateways;
use crate::secret::ClientSecret;
use crate::task::TaskGroup;
use futures::StreamExt as _;
use futures::stream::BoxStream;
use picomint_core::TransactionId;
use picomint_core::config::ConsensusConfig;
use picomint_core::config::MintId;
use picomint_core::core::{Account, OperationId};
use picomint_redb::{Database, WriteTx};

use crate::{TxAcceptEvent, TxRejectEvent};

/// The one per-mint context: API and gateway pools, the shared client
/// db, the mint config, the root secret, and the task group. Every
/// state machine runs against a clone of this, and every module operation
/// is a function over it — module configs, public key sets and per-module
/// secrets are projections (`config.ecash.tbs_pks`, `secret.ecash_secret()`),
/// never copies.
#[derive(Clone)]
pub struct ClientContext {
    pub(crate) api: MintApi,
    pub(crate) db: Database,
    pub(crate) config: ConsensusConfig,
    /// Memoized [`ConsensusConfig::calculate_mint_id`] — a consensus
    /// hash over the whole config, too hot to recompute per table key. Can
    /// never go stale: the config it is derived from is immutable beside it.
    pub(crate) mint: MintId,
    pub(crate) secret: ClientSecret,
    pub(crate) gateways: Gateways,
    pub(crate) tg: TaskGroup,
}

impl ClientContext {
    pub(crate) fn new(
        api: MintApi,
        db: Database,
        config: ConsensusConfig,
        secret: ClientSecret,
        gateways: Gateways,
        tg: TaskGroup,
    ) -> Self {
        Self {
            api,
            db,
            mint: config.calculate_mint_id(),
            config,
            secret,
            gateways,
            tg,
        }
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

    /// Stream every event belonging to `operation`, starting from the
    /// beginning of the log (existing events first, then live ones).
    pub fn subscribe_operation_events(
        &self,
        operation: OperationId,
    ) -> BoxStream<'static, EventLogEntry> {
        Box::pin(crate::eventlog::subscribe_operation_events(
            self.db.clone(),
            operation,
        ))
    }

    pub fn log_event<E>(&self, dbtx: &WriteTx, account: Account, operation: OperationId, event: E)
    where
        E: Event + Send,
    {
        crate::eventlog::log_event(dbtx, self.mint, account, operation, event);
    }
}
