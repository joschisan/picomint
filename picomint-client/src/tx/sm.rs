//! State machine for submitting transactions

use crate::context::ClientContext;
use picomint_core::config::MintId;
use picomint_core::core::{Account, OperationId};
use picomint_core::tx::Transaction;
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::{WriteTx, table};

use crate::executor::{SmId, StateMachine};
use crate::{TxAcceptEvent, TxRejectEvent};

table!(
    TxSubmissionStateMachineTable,
    (MintId, SmId) => TxSubmissionStateMachine,
    "tx-submission-sm",
);

/// State machine that submits a transaction and waits for the final outcome.
/// The server long-polls on `submit_tx`, returning either `Ok(())` once the
/// tx has been accepted or `Err(..)` once it has been definitively
/// invalidated.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Decodable, Encodable)]
pub struct TxSubmissionStateMachine {
    /// Account the transaction was built for. Only used to tag the accept /
    /// reject events, since submission itself is account-agnostic.
    pub account: Account,
    pub operation: OperationId,
    pub tx: Transaction,
}

impl StateMachine for TxSubmissionStateMachine {
    type Outcome = Result<(), String>;

    async fn trigger(&self, ctx: &ClientContext) -> Self::Outcome {
        crate::api::submit_tx(&ctx.api, self.tx.clone())
            .await
            .map_err(|e| e.to_string())
    }

    fn transition(
        &self,
        ctx: &ClientContext,
        dbtx: &WriteTx,
        outcome: Self::Outcome,
    ) -> Option<Self> {
        let txid = self.tx.compute_txid();

        match outcome {
            Ok(()) => {
                crate::eventlog::log_event(
                    dbtx,
                    ctx.mint,
                    self.account,
                    self.operation,
                    TxAcceptEvent { txid },
                );
            }
            Err(error) => {
                crate::eventlog::log_event(
                    dbtx,
                    ctx.mint,
                    self.account,
                    self.operation,
                    TxRejectEvent { txid, error },
                );
            }
        }
        None
    }
}
