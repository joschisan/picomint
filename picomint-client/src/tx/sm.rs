//! State machine for submitting transactions

use crate::api::FederationApi;
use picomint_core::config::FederationId;
use picomint_core::core::OperationId;
use picomint_core::tx::Transaction;
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::WriteTxRef;

use crate::executor::StateMachine;
use crate::{TxAcceptEvent, TxRejectEvent};

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
    pub federation_id: FederationId,
}

impl StateMachine for TxSubmissionStateMachine {
    const TABLE_NAME: &'static str = "tx-submission-sm";

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
        dbtx: &WriteTxRef<'_>,
        outcome: Self::Outcome,
    ) -> Option<Self> {
        let txid = self.tx.compute_txid();

        match outcome {
            Ok(()) => {
                picomint_eventlog::log_event(
                    dbtx,
                    ctx.federation_id,
                    self.operation,
                    TxAcceptEvent { txid },
                );
            }
            Err(error) => {
                picomint_eventlog::log_event(
                    dbtx,
                    ctx.federation_id,
                    self.operation,
                    TxRejectEvent { txid, error },
                );
            }
        }
        None
    }
}
