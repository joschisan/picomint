//! Federation expiry status caching.
//!
//! The federation's announced expiry is fetched once at federation bring-up
//! via threshold consensus and mirrored into the local
//! [`ExpiryStatusTable`]. [`Client::expiry_status`] is a fast, non-blocking
//! read from that cache; [`Client::refresh_expiry_status`] re-runs the
//! federation query on demand (used by tests and by apps that want to force
//! a re-sync).

use picomint_core::config::FederationId;
use picomint_core::expiry::ExpiryStatus;
use picomint_sqlite::{Database, DbRead, table};
use thiserror::Error;
use tracing::warn;

use crate::Client;
use crate::api::FederationApi;

table!(
    ExpiryStatusTable,
    FederationId => ExpiryStatus,
    "expiry-status",
);

#[derive(Error, Debug, Clone, Eq, PartialEq)]
pub enum RefreshExpiryStatusError {
    #[error("Failed to request expiry status")]
    FailedToRequestExpiryStatus,
}

impl Client {
    /// Read the cached expiry status. Populated at bring-up (and by
    /// [`Self::refresh_expiry_status`]); returns `None` until that completes
    /// successfully or if the federation has not announced an expiry. Pure
    /// read — never brings the federation up.
    pub fn expiry_status(&self, federation: FederationId) -> Option<ExpiryStatus> {
        self.db.begin_read().get(&ExpiryStatusTable, &federation)
    }

    /// Re-fetch the announced expiry via threshold consensus and reconcile
    /// the local cache. Brings the federation up.
    pub async fn refresh_expiry_status(
        &self,
        federation: FederationId,
    ) -> Result<(), RefreshExpiryStatusError> {
        let api = self
            .runtime(federation)
            .map_err(|_| RefreshExpiryStatusError::FailedToRequestExpiryStatus)?
            .api
            .clone();

        refresh_once(&api, &self.db, federation).await
    }
}

/// One-shot bring-up task: fetch the announced expiry and reconcile the
/// cache, logging instead of failing — the cache simply stays stale until
/// the next refresh.
pub(crate) async fn refresh(api: FederationApi, db: Database, federation: FederationId) {
    if refresh_once(&api, &db, federation).await.is_err() {
        warn!(%federation, "Failed to refresh the expiry status");
    }
}

async fn refresh_once(
    api: &FederationApi,
    db: &Database,
    federation: FederationId,
) -> Result<(), RefreshExpiryStatusError> {
    let status = api
        .expiry_status()
        .await
        .map_err(|_| RefreshExpiryStatusError::FailedToRequestExpiryStatus)?;

    let dbtx = db.begin_write();

    match status {
        Some(s) => {
            dbtx.insert(&ExpiryStatusTable, &federation, &s);
        }
        None => {
            dbtx.remove(&ExpiryStatusTable, &federation);
        }
    }

    dbtx.commit();

    Ok(())
}

/// Remove the federation's expiry cache row. Called on remove.
pub(crate) fn wipe_tables(dbtx: &picomint_sqlite::WriteTx, federation: FederationId) {
    dbtx.remove(&ExpiryStatusTable, &federation);
}
