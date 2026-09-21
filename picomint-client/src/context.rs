use crate::api::MintApi;
use crate::eventlog::{Event, EventLogEntry};
use crate::lightning::Gateways;
use crate::secret::ClientSecret;
use crate::swap::Brokers;
use crate::task::TaskGroup;
use futures::StreamExt as _;
use futures::stream::BoxStream;
use picomint_core::TransactionId;
use picomint_core::config::MintId;
use picomint_core::config::NodeConfigConsensus;
use picomint_core::core::{Account, OperationId};
use picomint_redb::{Database, WriteTx};
use std::collections::BTreeMap;
use std::sync::{RwLock, Weak};

use crate::{TxAcceptEvent, TxRejectEvent};

/// Every added mint's context, keyed by mint: the client's map, shared
/// with the contexts as a weak handle.
pub(crate) type Mints = RwLock<BTreeMap<MintId, ClientContext>>;

/// The one per-mint context: API, gateway and broker pools, the shared client
/// db, the mint config, the root secret, and the task group. Every
/// state machine runs against a clone of this, and every module operation
/// is a function over it — module configs, public key sets and per-module
/// secrets are projections (`config.ecash.tbs_pks`, `secret.ecash_secret()`),
/// never copies.
#[derive(Clone)]
pub struct ClientContext {
    pub(crate) api: MintApi,
    pub(crate) db: Database,
    pub(crate) config: NodeConfigConsensus,
    /// Memoized [`NodeConfigConsensus::calculate_mint_id`] — a consensus
    /// hash over the whole config, too hot to recompute per table key. Can
    /// never go stale: the config it is derived from is immutable beside it.
    pub(crate) mint: MintId,
    pub(crate) secret: ClientSecret,
    pub(crate) gateways: Gateways,
    pub(crate) brokers: Brokers,
    pub(crate) tg: TaskGroup,
    /// The client's mint map, for [`Self::sibling`]. Weak, since the map
    /// holds this context.
    mints: Weak<Mints>,
}

impl ClientContext {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        api: MintApi,
        db: Database,
        config: NodeConfigConsensus,
        secret: ClientSecret,
        gateways: Gateways,
        brokers: Brokers,
        tg: TaskGroup,
        mints: Weak<Mints>,
    ) -> Self {
        Self {
            api,
            db,
            mint: config.calculate_mint_id(),
            config,
            secret,
            gateways,
            brokers,
            tg,
            mints,
        }
    }

    /// The context of another added mint, for the one operation that spans
    /// two: a broker's swap, funded in one mint and claimed in another.
    /// `None` once the mint is removed, or the client is gone.
    pub(crate) fn sibling(&self, mint: MintId) -> Option<ClientContext> {
        self.mints
            .upgrade()?
            .read()
            .expect("mints lock poisoned")
            .get(&mint)
            .cloned()
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
