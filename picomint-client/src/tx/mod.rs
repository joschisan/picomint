mod builder;
mod sm;

pub use builder::*;
pub use picomint_core::tx::{ConsensusItem, Transaction, TxError};
pub use sm::*;

/// Remove every row this module owns under the caller's federation prefix.
/// Called by [`crate::Client::remove`] for end-of-life cleanup.
pub(crate) fn wipe_tables(
    dbtx: &picomint_sqlite::WriteTx,
    federation: picomint_core::config::FederationId,
) {
    dbtx.remove_prefix(&TxSubmissionStateMachineTable, &federation);
}
