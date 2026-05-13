//! Federation expiration status caching.
//!
//! The federation's announced expiration is fetched once at client startup
//! via threshold consensus and mirrored into the local
//! [`ExpirationStatusTable`]. [`Client::expiration_status`] is a fast,
//! non-blocking read from that cache; [`Client::refresh_expiration_status`]
//! re-runs the federation query on demand (used by tests and by apps that
//! want to force a re-sync).

use std::sync::Arc;

use picomint_core::config::FederationId;
use picomint_core::expiration::ExpirationStatus;
use thiserror::Error;

use crate::Client;

client_table!(
    ExpirationStatusTable,
    () => ExpirationStatus,
    "expiration-status",
);

#[derive(Error, Debug, Clone, Eq, PartialEq)]
pub enum RefreshExpirationStatusError {
    #[error("Failed to request expiration status")]
    FailedToRequestExpirationStatus,
}

impl Client {
    /// Read the cached expiration status. Populated by
    /// [`Self::refresh_expiration_status`] (run once at startup); returns
    /// `None` until that completes successfully or if the federation has
    /// not announced an expiration.
    pub fn expiration_status(&self) -> Option<ExpirationStatus> {
        self.db()
            .begin_read()
            .get(&ExpirationStatusTable(self.federation()), &())
    }

    /// Re-fetch the announced expiration via threshold consensus and
    /// reconcile the local cache. Inserts on `Some(_)`, removes on `None`.
    pub async fn refresh_expiration_status(
        client: Arc<Self>,
    ) -> Result<(), RefreshExpirationStatusError> {
        let status = client
            .api()
            .expiration_status()
            .await
            .map_err(|_| RefreshExpirationStatusError::FailedToRequestExpirationStatus)?;

        let dbtx = client.db().begin_write();

        match status {
            Some(s) => {
                dbtx.insert(&ExpirationStatusTable(client.federation()), &(), &s);
            }
            None => {
                dbtx.remove(&ExpirationStatusTable(client.federation()), &());
            }
        }

        dbtx.commit();

        Ok(())
    }
}

/// Drop the expiration cache table. Called by [`Client::wipe`].
pub(crate) fn wipe_tables(dbtx: &picomint_redb::WriteTxRef<'_>, federation: FederationId) {
    dbtx.delete_table(&ExpirationStatusTable(federation));
}
