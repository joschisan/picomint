//! Implements the client API through which users interact with the federation

use anyhow::Result;
use picomint_bitcoin_rpc::BitcoinRpcMonitor;
use picomint_core::expiration::ExpirationStatus;
use picomint_core::methods::CoreMethod;
use picomint_core::module::audit::AuditSummary;
use picomint_core::tx::{ConsensusItem, Transaction, TxError};

use crate::consensus::rpc;
use crate::{handler, handler_async};
use picomint_redb::Database;
use tokio::sync::watch::{Receiver, Sender};
use tracing::warn;

use crate::config::ServerConfig;
use crate::consensus::db::{
    AcceptedItemTable, AcceptedTxTable, ExpirationStatusTable, SignedSessionOutcomeTable,
};
use crate::consensus::engine::get_finished_session_count_static;
use crate::consensus::server::{Server, process_tx_with_server};
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
    pub submission_tx: async_channel::Sender<ConsensusItem>,
    pub shutdown_rx: Receiver<Option<u64>>,
    pub shutdown_tx: Sender<Option<u64>>,
    pub p2p_status_receivers: P2PStatusReceivers,
    pub bitcoin_rpc_connection: BitcoinRpcMonitor,
}

impl ConsensusApi {
    /// Submit a transaction and long-poll until it is either accepted by
    /// consensus or becomes invalid.
    pub async fn submit_tx(&self, tx: Transaction) -> Result<(), TxError> {
        let notify_item = self.db.notify_for_table(&AcceptedItemTable);
        let notify_session = self.db.notify_for_table(&SignedSessionOutcomeTable);

        let mut notified_item = Box::pin(notify_item.notified());
        let mut notified_session = Box::pin(notify_session.notified());

        let dbtx = self.db.begin_write();

        if dbtx.get(&AcceptedTxTable, &tx.compute_txid()).is_some() {
            return Ok(());
        }

        process_tx_with_server(&self.server, &dbtx, &tx).await?;

        drop(dbtx);

        if self
            .submission_tx
            .send(ConsensusItem::Tx(tx.clone()))
            .await
            .is_err()
        {
            warn!("Unable to submit the tx into consensus");
        }

        loop {
            tokio::select! {
                _ = &mut notified_item => {
                    let dbtx = self.db.begin_write();

                    if dbtx.get(&AcceptedTxTable, &tx.compute_txid()).is_some() {
                        return Ok(());
                    }

                    process_tx_with_server(&self.server, &dbtx, &tx).await?;

                    drop(dbtx);

                    notified_item = Box::pin(notify_item.notified());
                }
                _ = &mut notified_session => {
                    if self
                        .submission_tx
                        .send(ConsensusItem::Tx(tx.clone()))
                        .await
                        .is_err()
                    {
                        warn!("Unable to submit the tx into consensus");
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
        let dbtx = self.db.begin_write();
        self.server.audit(&dbtx).await
    }

    /// Read this guardian's announced expiration status from the local
    /// `ExpirationStatus` table. Returned over the wire by the
    /// `ExpirationStatus` RPC and surfaced on the dashboard.
    #[must_use]
    pub fn expiration_status_ui(&self) -> Option<ExpirationStatus> {
        self.db.begin_read().get(&ExpirationStatusTable, &())
    }

    /// Set or clear this guardian's announced expiration status. All
    /// guardians must announce byte-equal values for clients to accept the
    /// announcement (threshold-consensus read).
    pub fn set_expiration_status_ui(&self, status: Option<ExpirationStatus>) {
        let dbtx = self.db.begin_write();
        match status {
            Some(s) => {
                dbtx.insert(&ExpirationStatusTable, &(), &s);
            }
            None => {
                dbtx.remove(&ExpirationStatusTable, &());
            }
        }
        dbtx.commit();
    }
}

impl ConsensusApi {
    pub async fn handle_api(&self, method: CoreMethod) -> Result<Vec<u8>, String> {
        match method {
            CoreMethod::SubmitTx(req) => handler_async!(submit_tx, self, req).await,
            CoreMethod::Config(req) => handler!(config, self, req).await,
            CoreMethod::Liveness(req) => handler!(liveness, self, req).await,
            CoreMethod::ExpirationStatus(req) => handler!(expiration_status, self, req).await,
        }
    }
}
