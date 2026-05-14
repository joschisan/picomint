use crate::executor::{SmId, StateMachine};
use picomint_core::OutPoint;
use picomint_core::core::OperationId;
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::WriteTx;

use super::WalletClientContext;
use super::events::{SendFailureEvent, SendSuccessEvent};

crate::client_table!(
    SendStateMachineTable,
    SmId => SendStateMachine,
    "wallet-send-sm",
);

#[derive(Debug, Clone, Eq, PartialEq, Hash, Decodable, Encodable)]
pub struct SendStateMachine {
    pub operation: OperationId,
    pub outpoint: OutPoint,
    pub amount: bitcoin::Amount,
    pub fee: bitcoin::Amount,
}

picomint_redb::consensus_value!(SendStateMachine);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AwaitFundingResult {
    Success(bitcoin::Txid),
    Aborted(String),
    Failure,
}

impl StateMachine for SendStateMachine {
    type Context = WalletClientContext;
    type Outcome = AwaitFundingResult;

    async fn trigger(&self, ctx: &Self::Context) -> Self::Outcome {
        if let Err(error) = ctx
            .client_ctx
            .await_tx_accepted(self.operation, self.outpoint.txid)
            .await
        {
            return AwaitFundingResult::Aborted(error);
        }

        match ctx.client_ctx.api().wallet_tx_id(self.outpoint).await {
            Some(txid) => AwaitFundingResult::Success(txid),
            None => AwaitFundingResult::Failure,
        }
    }

    fn transition(
        &self,
        ctx: &Self::Context,
        dbtx: &WriteTx,
        outcome: Self::Outcome,
    ) -> Option<Self> {
        match outcome {
            AwaitFundingResult::Success(txid) => {
                ctx.client_ctx
                    .log_event(dbtx, self.operation, SendSuccessEvent { txid });
            }
            AwaitFundingResult::Aborted(_) => {}
            AwaitFundingResult::Failure => {
                ctx.client_ctx
                    .log_event(dbtx, self.operation, SendFailureEvent);
            }
        }

        None
    }
}
