mod builder;
mod sm;

use std::sync::Arc;

pub use builder::*;
use picomint_core::config::FederationId;
use picomint_core::core::OperationId;
pub use picomint_core::tx::{ConsensusItem, Transaction, TxError};
use picomint_redb::{Database, DbRead, ReadTx, WriteTx};
pub use sm::*;
use tokio::sync::Notify;

/// Remove every row this module owns under the caller's federation prefix.
/// Called by [`crate::Client::remove`] for end-of-life cleanup.
pub(crate) fn wipe_tables(dbtx: &WriteTx, federation: FederationId) {
    dbtx.remove_prefix(&TxSubmissionStateMachineTable, &federation);
}

/// Whether any of this module's state machines for `operation` is still
/// active under `federation`.
pub(crate) fn operation_is_active(
    dbtx: &ReadTx,
    federation: FederationId,
    operation: OperationId,
) -> bool {
    dbtx.prefix(&TxSubmissionStateMachineTable, &federation, |r| {
        r.any(|entry| entry.1.operation == operation)
    })
}

/// Notify handles for this module's state machine tables, fired on every
/// commit that writes them.
pub(crate) fn sm_notifies(db: &Database) -> Vec<Arc<Notify>> {
    vec![db.notify_for_table(&TxSubmissionStateMachineTable)]
}
