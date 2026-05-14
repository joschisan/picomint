mod builder;
mod sm;

pub use builder::*;
pub use picomint_core::tx::{ConsensusItem, Transaction, TxError};
pub use sm::*;

/// Drop every redb table this module owns under the caller's prefix.
/// Called by [`crate::Client::wipe`] for end-of-life client cleanup.
pub(crate) fn wipe_tables(
    dbtx: &picomint_redb::WriteTx,
    federation: picomint_core::config::FederationId,
) {
    dbtx.delete_table(&TxSubmissionStateMachineTable(federation));
}
