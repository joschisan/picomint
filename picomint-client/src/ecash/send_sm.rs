use futures::StreamExt;
use picomint_core::Amount;
use picomint_core::config::MintId;
use picomint_core::core::{Account, OperationId};
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::{WriteTx, table};

use crate::TxRejectEvent;
use crate::executor::{SmId, StateMachine};

use super::events::{EcashFailureEvent, EcashSuccessEvent, SendFailureEvent, SendSuccessEvent};
use crate::context::ClientContext;

table!(
    SendStateMachineTable,
    (MintId, SmId) => SendStateMachine,
    "ecash-send-sm",
);

/// Drives the slow-path tail of [`crate::Client::ecash_send`]. The reissuance tx and
/// `EcashStateMachine` are wired up in the same dbtx that submits the
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
    /// `EcashSuccessEvent` landed — the freshly reissued notes are in
    /// `NoteTable`, attempt assembly.
    Success,
    /// `TxRejectEvent` or `EcashFailureEvent` landed — reissuance is
    /// dead, the send can't complete.
    Failure,
}

impl StateMachine for SendStateMachine {
    type Outcome = SendOutcome;

    async fn trigger(&self, ctx: &ClientContext) -> Self::Outcome {
        let mut stream = ctx.subscribe_operation_events(self.operation);
        while let Some(entry) = stream.next().await {
            if entry.to_event::<EcashSuccessEvent>().is_some() {
                return SendOutcome::Success;
            }
            if entry.to_event::<EcashFailureEvent>().is_some() {
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
                match super::send_ecash_dbtx(dbtx, ctx.mint, self.account, self.amount) {
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
