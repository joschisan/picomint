use futures::StreamExt;
use picomint_core::Amount;
use picomint_core::config::FederationId;
use picomint_core::core::{Account, OperationId};
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::{WriteTx, table};

use crate::TxRejectEvent;
use crate::executor::{SmId, StateMachine};

use super::events::{MintFailureEvent, MintSuccessEvent, SendFailureEvent, SendSuccessEvent};
use crate::module::ClientContext;

table!(
    SendStateMachineTable,
    (FederationId, SmId) => SendStateMachine,
    "mint-send-sm",
);

/// Drives the slow-path tail of `mint().send()`. The reissuance tx and
/// `MintStateMachine` are wired up in the same dbtx that submits the
/// remint; this SM observes the operation's terminal events and either
/// assembles the requested ecash from the freshly minted notes or logs
/// `SendFailureEvent`.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Decodable, Encodable)]
pub struct SendStateMachine {
    /// Account the reissued notes land in, and therefore the one the bundle
    /// is assembled from.
    pub account: Account,
    pub operation: OperationId,
    pub amount: Amount,
}

#[derive(Debug)]
pub enum SendOutcome {
    /// `MintSuccessEvent` landed — the freshly reissued notes are in
    /// `NoteTable`, attempt assembly.
    Success,
    /// `TxRejectEvent` or `MintFailureEvent` landed — reissuance is
    /// dead, the send can't complete.
    Failure,
}

impl StateMachine for SendStateMachine {
    type Outcome = SendOutcome;

    async fn trigger(&self, ctx: &ClientContext) -> Self::Outcome {
        let mut stream = ctx.subscribe_operation_events(self.operation);
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
        ctx: &ClientContext,
        dbtx: &WriteTx,
        outcome: Self::Outcome,
    ) -> Option<Self> {
        match outcome {
            SendOutcome::Success => {
                match super::send_ecash_dbtx(dbtx, ctx.federation, self.account, self.amount) {
                    Some(ecash) => ctx.log_event(
                        dbtx,
                        self.account,
                        self.operation,
                        SendSuccessEvent {
                            ecash: ecash.to_string(),
                        },
                    ),
                    None => ctx.log_event(dbtx, self.account, self.operation, SendFailureEvent),
                }
            }
            SendOutcome::Failure => {
                ctx.log_event(dbtx, self.account, self.operation, SendFailureEvent)
            }
        }
        None
    }
}
