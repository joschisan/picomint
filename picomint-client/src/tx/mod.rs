mod builder;
mod sm;

pub use builder::*;
use picomint_core::config::FederationId;
pub use picomint_core::tx::{ConsensusItem, Transaction, TxError};
use picomint_sqlite::WriteTx;
pub use sm::*;

/// Remove every row this module owns under the caller's federation prefix.
/// Called by [`crate::Client::remove`] for end-of-life cleanup.
pub(crate) fn wipe_tables(dbtx: &WriteTx, federation: FederationId) {
    dbtx.remove_prefix(&TxSubmissionStateMachineTable, &federation);
}
