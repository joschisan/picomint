use futures::StreamExt;
use picomint_core::Amount;
use picomint_core::core::OperationId;
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::WriteTxRef;

use crate::TxRejectEvent;
use crate::executor::StateMachine;

use super::MintSmContext;
use super::events::{MintFailureEvent, MintSuccessEvent, SendFailureEvent, SendSuccessEvent};

/// Drives the slow-path tail of `mint().send()`. The reissuance tx and
/// `MintStateMachine` are wired up in the same dbtx that submits the
/// remint; this SM observes the operation's terminal events and either
/// assembles the requested ecash from the freshly minted notes or logs
/// `SendFailureEvent`.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Decodable, Encodable)]
pub struct SendStateMachine {
    pub operation: OperationId,
    pub amount: Amount,
}

picomint_redb::consensus_value!(SendStateMachine);

#[derive(Debug)]
pub enum SendOutcome {
    /// `MintSuccessEvent` landed — the freshly reissued notes are in
    /// `NOTE`, attempt assembly.
    Success,
    /// `TxRejectEvent` or `MintFailureEvent` landed — reissuance is
    /// dead, the send can't complete.
    Failure,
}

impl StateMachine for SendStateMachine {
    const TABLE_NAME: &'static str = "mint-send-sm";

    type Context = MintSmContext;
    type Outcome = SendOutcome;

    async fn trigger(&self, ctx: &Self::Context) -> Self::Outcome {
        let mut stream = ctx.client_ctx.subscribe_operation_events(self.operation);
        while let Some(entry) = stream.next().await {
            if entry.to_event::<MintSuccessEvent>().is_some() {
                return SendOutcome::Success;
            }
            if entry.to_event::<MintFailureEvent>().is_some() {
                return SendOutcome::Failure;
            }
            if entry.to_event::<TxRejectEvent>().is_some() {
                return SendOutcome::Failure;
            }
        }
        unreachable!("subscribe_operation_events only ends at client shutdown")
    }

    fn transition(
        &self,
        ctx: &Self::Context,
        dbtx: &WriteTxRef<'_>,
        outcome: Self::Outcome,
    ) -> Option<Self> {
        match outcome {
            SendOutcome::Success => {
                match super::send_ecash_dbtx(dbtx, ctx.federation_id, self.amount) {
                    Some(ecash) => {
                        ctx.client_ctx
                            .log_event(dbtx, self.operation, SendSuccessEvent { ecash })
                    }
                    None => ctx
                        .client_ctx
                        .log_event(dbtx, self.operation, SendFailureEvent),
                }
            }
            SendOutcome::Failure => {
                ctx.client_ctx
                    .log_event(dbtx, self.operation, SendFailureEvent)
            }
        }
        None
    }
}
