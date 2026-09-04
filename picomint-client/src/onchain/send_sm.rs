use crate::context::ClientContext;
use crate::executor::{SmId, StateMachine};
use picomint_core::OutPoint;
use picomint_core::config::MintId;
use picomint_core::core::{Account, OperationId};
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::{WriteTx, table};

use super::events::{SendFailureEvent, SendSuccessEvent};

table!(
    SendStateMachineTable,
    (MintId, SmId) => SendStateMachine,
    "onchain-send-sm",
);

#[derive(Debug, Clone, Eq, PartialEq, Hash, Decodable, Encodable)]
pub struct SendStateMachine {
    pub operation: OperationId,
    /// Account that funded the send. Only used to tag this SM's events.
    pub account: Account,
    pub outpoint: OutPoint,
    pub amount: bitcoin::Amount,
    pub fee: bitcoin::Amount,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AwaitFundingResult {
    Success(bitcoin::Txid),
    Aborted(String),
    Failure,
}

impl StateMachine for SendStateMachine {
    type Outcome = AwaitFundingResult;

    async fn trigger(&self, ctx: &ClientContext) -> Self::Outcome {
        if let Err(error) = ctx
            .await_tx_accepted(self.operation, self.outpoint.txid)
            .await
        {
            return AwaitFundingResult::Aborted(error);
        }

        match super::api::tx_id(&ctx.api, self.outpoint).await {
            Some(txid) => AwaitFundingResult::Success(txid),
            None => AwaitFundingResult::Failure,
        }
    }

    fn transition(
        &self,
        ctx: &ClientContext,
        dbtx: &WriteTx,
        outcome: Self::Outcome,
    ) -> Option<Self> {
        match outcome {
            AwaitFundingResult::Success(txid) => {
                ctx.log_event(
                    dbtx,
                    self.account,
                    self.operation,
                    SendSuccessEvent { txid },
                );
            }
            AwaitFundingResult::Aborted(_) => {}
            AwaitFundingResult::Failure => {
                ctx.log_event(dbtx, self.account, self.operation, SendFailureEvent);
            }
        }

        None
    }
}
