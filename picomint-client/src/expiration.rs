//! Federation expiration status caching.
//!
//! The federation's announced expiration is fetched once at client startup
//! via threshold consensus and mirrored into the local
//! [`EXPIRATION_STATUS`] table. [`Client::expiration_status`] is a fast,
//! non-blocking read from that cache; [`Client::refresh_expiration_status`]
//! re-runs the federation query on demand (used by tests and by apps that
//! want to force a re-sync).

use picomint_core::expiration::ExpirationStatus;
use picomint_redb::table;
use thiserror::Error;

use crate::Client;

table!(
    EXPIRATION_STATUS,
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
        self.db().begin_read().get(&EXPIRATION_STATUS, &())
    }

    /// Re-fetch the announced expiration via threshold consensus and
    /// reconcile the local cache. Inserts on `Some(_)`, removes on `None`.
    pub async fn refresh_expiration_status(&self) -> Result<(), RefreshExpirationStatusError> {
        let status = self
            .api()
            .expiration_status()
            .await
            .map_err(|_| RefreshExpirationStatusError::FailedToRequestExpirationStatus)?;

        let dbtx = self.db().begin_write();

        match status {
            Some(s) => {
                dbtx.insert(&EXPIRATION_STATUS, &(), &s);
            }
            None => {
                dbtx.remove(&EXPIRATION_STATUS, &());
            }
        }

        dbtx.commit();

        Ok(())
    }
}

/// Drop the expiration cache table. Called by [`Client::wipe`].
pub(crate) fn wipe_tables(dbtx: &picomint_redb::WriteTxRef<'_>) {
    dbtx.delete_table(&EXPIRATION_STATUS);
}
