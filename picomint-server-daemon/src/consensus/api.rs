//! Implements the client API through which users interact with the federation

use std::collections::BTreeMap;

use anyhow::Result;
use picomint_bitcoin_rpc::BitcoinRpcMonitor;
use picomint_core::methods::CoreMethod;
use picomint_core::module::ApiError;
use picomint_core::module::audit::AuditSummary;
use picomint_core::transaction::{ConsensusItem, Transaction, TransactionError};

use crate::consensus::rpc;
use picomint_core::PeerId;
use picomint_core::task::TaskGroup;
use picomint_iroh_api::{handler, handler_async};
use picomint_logging::LOG_NET_API;
use picomint_redb::Database;
use tokio::sync::watch::{Receiver, Sender};
use tracing::warn;

use crate::config::ServerConfig;
use crate::consensus::db::{ACCEPTED_ITEM, ACCEPTED_TRANSACTION, SIGNED_SESSION_OUTCOME};
use crate::consensus::engine::get_finished_session_count_static;
use crate::consensus::server::{Server, process_transaction_with_server};
use crate::p2p::P2PStatusReceivers;

#[derive(Clone)]
pub struct ConsensusApi {
    /// Our server configuration
    pub cfg: ServerConfig,
    /// Database for serving the API
    pub db: Database,
    /// Static wire-dispatch handle to the fixed module set
    pub server: Server,
    /// For sending API events to consensus such as transactions
    pub submission_sender: async_channel::Sender<ConsensusItem>,
    pub shutdown_receiver: Receiver<Option<u64>>,
    pub shutdown_sender: Sender<Option<u64>>,
    pub p2p_status_receivers: P2PStatusReceivers,
    pub ci_status_receivers: BTreeMap<PeerId, Receiver<Option<u64>>>,
    pub bitcoin_rpc_connection: BitcoinRpcMonitor,
    pub task_group: TaskGroup,
}

impl ConsensusApi {
    /// Submit a transaction and long-poll until it is either accepted by
    /// consensus or becomes invalid.
    pub async fn submit_transaction(
        &self,
        transaction: Transaction,
    ) -> Result<(), TransactionError> {
        let notify_item = self.db.notify_for_table(&ACCEPTED_ITEM);
        let notify_session = self.db.notify_for_table(&SIGNED_SESSION_OUTCOME);

        let mut notified_item = Box::pin(notify_item.notified());
        let mut notified_session = Box::pin(notify_session.notified());

        let tx = self.db.begin_write();

        if tx
            .get(&ACCEPTED_TRANSACTION, &transaction.tx_hash())
            .is_some()
        {
            return Ok(());
        }

        process_transaction_with_server(&self.server, &tx, &transaction).await?;

        drop(tx);

        if self
            .submission_sender
            .send(ConsensusItem::Transaction(transaction.clone()))
            .await
            .is_err()
        {
            warn!(target: LOG_NET_API, "Unable to submit the tx into consensus");
        }

        loop {
            tokio::select! {
                _ = &mut notified_item => {
                    let tx = self.db.begin_write();

                    if tx
                        .get(&ACCEPTED_TRANSACTION, &transaction.tx_hash())
                        .is_some()
                    {
                        return Ok(());
                    }

                    process_transaction_with_server(&self.server, &tx, &transaction).await?;

                    drop(tx);

                    notified_item = Box::pin(notify_item.notified());
                }
                _ = &mut notified_session => {
                    if self
                        .submission_sender
                        .send(ConsensusItem::Transaction(transaction.clone()))
                        .await
                        .is_err()
                    {
                        warn!(target: LOG_NET_API, "Unable to submit the tx into consensus");
                    }

                    notified_session = Box::pin(notify_session.notified());
                }
            }
        }
    }

    pub async fn session_count(&self) -> u64 {
        get_finished_session_count_static(&self.db.begin_read()).await
    }

    pub async fn federation_audit(&self) -> AuditSummary {
        // Modules read their own tables during `audit`; we open a write tx and
        // drop it without commit after building the audit view.
        let tx = self.db.begin_write();
        self.server.audit(&tx).await
    }
}

impl ConsensusApi {
    pub async fn handle_api(&self, method: CoreMethod) -> Result<Vec<u8>, ApiError> {
        match method {
            CoreMethod::SubmitTransaction(req) => {
                handler_async!(submit_transaction, self, req).await
            }
            CoreMethod::Config(req) => handler!(config, self, req).await,
            CoreMethod::Liveness(req) => handler!(liveness, self, req).await,
        }
    }
}
