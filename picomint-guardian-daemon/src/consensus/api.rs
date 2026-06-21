//! Implements the client API through which users interact with the federation

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use chrono::{Days, Utc};
use picomint_bitcoin_rpc::BitcoinRpcMonitor;
use picomint_core::expiry::ExpiryStatus;
use picomint_core::invite::InviteCode;
use picomint_core::methods::CoreMethod;
use picomint_core::module::audit::AuditSummary;
use picomint_core::tx::{ConsensusItem, Transaction, TxError};

use crate::consensus::rpc;
use crate::{handler, handler_async};
use picomint_redb::Database;
use tokio::sync::watch::{Receiver, Sender};
use tracing::{info, warn};

use crate::config::ServerConfig;
use crate::consensus::db::{
    AcceptedItemTable, AcceptedTxTable, ExpiryStatusTable, InviteMeta, InviteMetaTable,
    InviteUserCountTable, SignedSessionOutcomeTable,
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
    /// consensus or becomes invalid. On acceptance, logs the wall-clock from
    /// submission to confirmation, so the server side of client-observed
    /// latency can be profiled straight from the guardian's `info` logs.
    pub async fn submit_tx(&self, tx: Transaction) -> Result<(), TxError> {
        let start = Instant::now();

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
                        info!(
                            txid = %tx.compute_txid(),
                            elapsed_ms = start.elapsed().as_millis() as u64,
                            "Submission RPC confirmed tx",
                        );

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

    /// Generate a fresh invite code expiring `expiry_days` from now and
    /// onboarding up to `user_limit` users, registering its [`InviteMeta`] in
    /// the local database so this guardian can enforce both when serving the
    /// config. Returns the code together with that metadata for display.
    pub fn create_invite_code(
        &self,
        expiry_days: u64,
        user_limit: u64,
    ) -> (InviteCode, InviteMeta) {
        let expires_at = Utc::now()
            .checked_add_days(Days::new(expiry_days))
            .expect("adding the expiry to the current date cannot overflow")
            .timestamp()
            .try_into()
            .expect("a future timestamp is positive");

        let meta = InviteMeta {
            expires_at,
            user_limit,
        };

        let invite_id = rand::random::<[u8; 16]>();

        let dbtx = self.db.begin_write();

        dbtx.insert(&InviteMetaTable, &invite_id, &meta);

        dbtx.commit();

        (self.cfg.get_invite_code(invite_id), meta)
    }

    /// Check the expiration date and user limit of the invite code with this
    /// invite id and count the download towards its user limit. Returns an
    /// error string (surfaced to the client) for unknown, expired, or
    /// exhausted invite codes.
    pub fn register_config_download(&self, invite_id: [u8; 16]) -> Result<(), String> {
        let dbtx = self.db.begin_write();

        let meta = dbtx
            .get(&InviteMetaTable, &invite_id)
            .ok_or_else(|| "Unknown invite id".to_string())?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the unix epoch")
            .as_secs();

        if meta.expires_at <= now {
            return Err("Invite code is expired".to_string());
        }

        let users = dbtx.get(&InviteUserCountTable, &invite_id).unwrap_or(0);

        if users >= meta.user_limit {
            return Err("Invite code has reached its user limit".to_string());
        }

        dbtx.insert(&InviteUserCountTable, &invite_id, &(users + 1));

        dbtx.commit();

        Ok(())
    }

    pub async fn federation_audit(&self) -> AuditSummary {
        // Modules read their own tables during `audit`; we open a write tx and
        // drop it without commit after building the audit view.
        let dbtx = self.db.begin_write();
        self.server.audit(&dbtx).await
    }

    /// Read this guardian's announced expiry status from the local
    /// `ExpiryStatus` table. Returned over the wire by the
    /// `ExpiryStatus` RPC and surfaced on the dashboard.
    #[must_use]
    pub fn expiry_status_ui(&self) -> Option<ExpiryStatus> {
        self.db.begin_read().get(&ExpiryStatusTable, &())
    }

    /// Set or clear this guardian's announced expiry status. All
    /// guardians must announce byte-equal values for clients to accept the
    /// announcement (threshold-consensus read).
    pub fn set_expiry_status_ui(&self, status: Option<ExpiryStatus>) {
        let dbtx = self.db.begin_write();
        match status {
            Some(s) => {
                dbtx.insert(&ExpiryStatusTable, &(), &s);
            }
            None => {
                dbtx.remove(&ExpiryStatusTable, &());
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
            CoreMethod::ExpiryStatus(req) => handler!(expiry_status, self, req).await,
        }
    }
}
