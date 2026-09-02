//! Implements the client API through which users interact with the federation

use chrono::{Days, Utc};
use picomint_core::expiry::ExpiryStatus;
use picomint_core::invite::InviteCode;
use picomint_core::module::audit::AuditSummary;
use picomint_core::tx::ConsensusItem;

use picomint_redb::DbRead;

use crate::consensus::db::{ExpiryStatusTable, InviteMeta, InviteMetaTable};
use crate::consensus::engine::get_finished_session_count;
use crate::consensus::server::Server;
use crate::p2p::P2PStatusReceivers;

#[derive(Clone)]
pub struct ConsensusApi {
    /// The shared server context: config, database, bitcoin backend
    pub server: Server,
    /// For sending API events to consensus such as transactions
    pub submission_tx: async_channel::Sender<ConsensusItem>,
    pub p2p_status_receivers: P2PStatusReceivers,
}

impl ConsensusApi {
    pub fn session_count(&self) -> u64 {
        get_finished_session_count(&self.server.db.begin_read())
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

        let dbtx = self.server.db.begin_write();

        dbtx.insert(&InviteMetaTable, &invite_id, &meta);

        dbtx.commit();

        (self.server.cfg.get_invite_code(invite_id), meta)
    }

    pub fn federation_audit(&self) -> AuditSummary {
        // Modules read their own tables during `audit`; we open a write tx and
        // drop it without commit after building the audit view.
        self.server.audit(&self.server.db.begin_write())
    }

    /// Read this guardian's announced expiry status from the local
    /// `ExpiryStatus` table. Returned over the wire by the
    /// `ExpiryStatus` RPC and surfaced on the dashboard.
    #[must_use]
    pub fn expiry_status(&self) -> Option<ExpiryStatus> {
        self.server.db.begin_read().get(&ExpiryStatusTable, &())
    }

    /// Set or clear this guardian's announced expiry status. All
    /// guardians must announce byte-equal values for clients to accept the
    /// announcement (threshold-consensus read).
    pub fn set_expiry_status(&self, status: Option<ExpiryStatus>) {
        let dbtx = self.server.db.begin_write();
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
