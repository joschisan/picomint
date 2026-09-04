//! Mint expiry status caching.
//!
//! The mint's announced expiry is fetched once at mint bring-up
//! via threshold consensus and mirrored into the local
//! [`ExpiryStatusTable`]. [`Client::expiry_status`] is a fast, non-blocking
//! read from that cache; [`Client::refresh_expiry_status`] re-runs the
//! mint query on demand (used by tests and by apps that want to force
//! a re-sync).

use picomint_core::config::MintId;
use picomint_core::expiry::ExpiryStatus;
use picomint_redb::{DbRead, WriteTx, table};
use thiserror::Error;
use tracing::warn;

use crate::Client;
use crate::context::ClientContext;

table!(
    ExpiryStatusTable,
    MintId => ExpiryStatus,
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
    /// successfully or if the mint has not announced an expiry. Pure
    /// read.
    pub fn expiry_status(&self, mint: MintId) -> Option<ExpiryStatus> {
        self.db.begin_read().get(&ExpiryStatusTable, &mint)
    }

    /// Re-fetch the announced expiry via threshold consensus and reconcile
    /// the local cache.
    pub async fn refresh_expiry_status(
        &self,
        mint: MintId,
    ) -> Result<(), RefreshExpiryStatusError> {
        let ctx = self
            .ctx(mint)
            .map_err(|_| RefreshExpiryStatusError::FailedToRequestExpiryStatus)?;

        refresh_once(&ctx).await
    }
}

/// One-shot bring-up task: fetch the announced expiry and reconcile the
/// cache, logging instead of failing — the cache simply stays stale until
/// the next refresh.
pub(crate) async fn refresh(ctx: ClientContext) {
    if refresh_once(&ctx).await.is_err() {
        warn!(mint = %ctx.mint, "Failed to refresh the expiry status");
    }
}

async fn refresh_once(ctx: &ClientContext) -> Result<(), RefreshExpiryStatusError> {
    let status = crate::api::expiry_status(&ctx.api)
        .await
        .map_err(|_| RefreshExpiryStatusError::FailedToRequestExpiryStatus)?;

    let dbtx = ctx.db.begin_write();

    match status {
        Some(s) => {
            dbtx.insert(&ExpiryStatusTable, &ctx.mint, &s);
        }
        None => {
            dbtx.remove(&ExpiryStatusTable, &ctx.mint);
        }
    }

    dbtx.commit();

    Ok(())
}

/// Remove the mint's expiry cache row. Called on remove.
pub(crate) fn wipe_tables(dbtx: &WriteTx, mint: MintId) {
    dbtx.remove(&ExpiryStatusTable, &mint);
}
