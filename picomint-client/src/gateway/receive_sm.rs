use picomint_redb::{WriteTx, table};

use picomint_core::OutPoint;
use picomint_core::config::MintId;
use picomint_core::core::OperationId;
use picomint_encoding::{Decodable, Encodable};

use super::events::{ReceiveFailureEvent, ReceiveSuccessEvent};
use crate::context::ClientContext;
use crate::executor::{SmId, StateMachine};

table!(
    ReceiveStateMachineTable,
    (MintId, SmId) => ReceiveStateMachine,
    "gateway-receive-sm",
);

/// Single-state state machine covering the mint side of the receive
/// flow. `trigger` waits for the funding transaction's acceptance;
/// `transition` logs the terminal receive event. The preimage is the
/// gateway's own, made with the invoice, so acceptance alone is what
/// releases it to a direct swap's sender, by the trailer task watching
/// the event log; an inbound HTLC was settled with it when the funding
/// was submitted.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Decodable, Encodable)]
pub struct ReceiveStateMachine {
    pub operation: OperationId,
    pub outpoint: OutPoint,
    pub preimage: [u8; 32],
}

impl StateMachine for ReceiveStateMachine {
    type Outcome = Result<(), String>;

    async fn trigger(&self, ctx: &ClientContext) -> Self::Outcome {
        ctx.await_tx_accepted(self.operation, self.outpoint.txid)
            .await
    }

    fn transition(
        &self,
        ctx: &ClientContext,
        dbtx: &WriteTx,
        outcome: Self::Outcome,
    ) -> Option<Self> {
        match outcome {
            Ok(()) => ctx.log_event(
                dbtx,
                super::ROUTING_ACCOUNT,
                self.operation,
                ReceiveSuccessEvent {
                    preimage: self.preimage,
                },
            ),
            Err(_) => ctx.log_event(
                dbtx,
                super::ROUTING_ACCOUNT,
                self.operation,
                ReceiveFailureEvent,
            ),
        }

        None
    }
}
