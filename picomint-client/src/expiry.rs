//! Federation expiry status caching.
//!
//! The federation's announced expiry is fetched once at client startup
//! via threshold consensus and mirrored into the local
//! [`ExpiryStatusTable`]. [`Client::expiry_status`] is a fast,
//! non-blocking read from that cache; [`Client::refresh_expiry_status`]
//! re-runs the federation query on demand (used by tests and by apps that
//! want to force a re-sync).

use std::sync::Arc;

use picomint_core::config::FederationId;
use picomint_core::expiry::ExpiryStatus;
use thiserror::Error;

use crate::Client;

client_table!(
    ExpiryStatusTable,
    () => ExpiryStatus,
    "expiry-status",
);

#[derive(Error, Debug, Clone, Eq, PartialEq)]
pub enum RefreshExpiryStatusError {
    #[error("Failed to request expiry status")]
    FailedToRequestExpiryStatus,
}

impl Client {
    /// Read the cached expiry status. Populated by
    /// [`Self::refresh_expiry_status`] (run once at startup); returns
    /// `None` until that completes successfully or if the federation has
    /// not announced an expiry.
    pub fn expiry_status(&self) -> Option<ExpiryStatus> {
        self.db()
            .begin_read()
            .get(&ExpiryStatusTable(self.federation()), &())
    }

    /// Re-fetch the announced expiry via threshold consensus and
    /// reconcile the local cache. Inserts on `Some(_)`, removes on `None`.
    pub async fn refresh_expiry_status(client: Arc<Self>) -> Result<(), RefreshExpiryStatusError> {
        let status = client
            .api()
            .expiry_status()
            .await
            .map_err(|_| RefreshExpiryStatusError::FailedToRequestExpiryStatus)?;

        let dbtx = client.db().begin_write();

        match status {
            Some(s) => {
                dbtx.insert(&ExpiryStatusTable(client.federation()), &(), &s);
            }
            None => {
                dbtx.remove(&ExpiryStatusTable(client.federation()), &());
            }
        }

        dbtx.commit();

        Ok(())
    }
}

/// Drop the expiry cache table. Called by [`Client::wipe`].
pub(crate) fn wipe_tables(dbtx: &picomint_redb::WriteTx, federation: FederationId) {
    dbtx.delete_table(&ExpiryStatusTable(federation));
}
