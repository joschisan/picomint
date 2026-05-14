//! State machine for submitting transactions

use crate::api::FederationApi;
use picomint_core::config::FederationId;
use picomint_core::core::OperationId;
use picomint_core::tx::Transaction;
use picomint_encoding::{Decodable, Encodable};
use picomint_eventlog::EventLogger;
use picomint_redb::WriteTx;

use crate::executor::{SmId, StateMachine};
use crate::{TxAcceptEvent, TxRejectEvent};

crate::client_table!(
    TxSubmissionStateMachineTable,
    SmId => TxSubmissionStateMachine,
    "tx-submission-sm",
);

/// State machine that submits a transaction and waits for the final outcome.
/// The server long-polls on `submit_tx`, returning either `Ok(())` once the
/// tx has been accepted or `Err(..)` once it has been definitively
/// invalidated.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Decodable, Encodable)]
pub struct TxSubmissionStateMachine {
    pub operation: OperationId,
    pub tx: Transaction,
}

picomint_redb::consensus_value!(TxSubmissionStateMachine);

/// Context for running [`TxSubmissionStateMachine`] in a typed
/// [`crate::executor::ModuleExecutor`].
#[derive(Debug, Clone)]
pub struct TxSubmissionSmContext {
    pub api: FederationApi,
    pub federation: FederationId,
    pub logger: EventLogger,
}

impl StateMachine for TxSubmissionStateMachine {
    type Context = TxSubmissionSmContext;
    type Outcome = Result<(), String>;

    async fn trigger(&self, ctx: &Self::Context) -> Self::Outcome {
        ctx.api
            .submit_tx(self.tx.clone())
            .await
            .map_err(|e| e.to_string())
    }

    fn transition(
        &self,
        ctx: &Self::Context,
        dbtx: &WriteTx,
        outcome: Self::Outcome,
    ) -> Option<Self> {
        let txid = self.tx.compute_txid();

        match outcome {
            Ok(()) => {
                ctx.logger
                    .log_event(dbtx, ctx.federation, self.operation, TxAcceptEvent { txid });
            }
            Err(error) => {
                ctx.logger.log_event(
                    dbtx,
                    ctx.federation,
                    self.operation,
                    TxRejectEvent { txid, error },
                );
            }
        }
        None
    }
}
