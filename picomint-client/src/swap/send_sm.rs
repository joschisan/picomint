use futures::future::pending;
use picomint_core::OutPoint;
use picomint_core::TransactionId;
use picomint_core::config::MintId;
use picomint_core::core::{Account, OperationId};
use picomint_core::swap::broker::BrokerPk;
use picomint_core::swap::methods::{BrokerMethod, SwapRequest, SwapResponse};
use picomint_core::swap::{ReceiveContract, SendContract};
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::{WriteTx, table};
use tbs::Signature;
use tracing::{instrument, warn};

use super::Brokers;
use super::events::SendSuccessEvent;
use crate::context::ClientContext;
use crate::executor::{SmId, StateMachine};

table!(
    SendStateMachineTable,
    (MintId, SmId) => SendStateMachine,
    "swap-send-sm",
);

/// Single-state state machine covering a send through a broker: the send
/// contract is locked, so all that is left is the broker's attestation. A
/// send contract has no refund path, so there is nothing to do but wait —
/// the request is reissued on every reconnect and a broker is idempotent
/// on the send outpoint.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Decodable, Encodable)]
pub struct SendStateMachine {
    pub account: Account,
    pub operation: OperationId,
    pub outpoint: OutPoint,
    pub send: SendContract,
    pub receive: ReceiveContract,
    pub broker: BrokerPk,
}

pub enum SendOutcome {
    Rejected,
    Attested(Signature),
}

impl StateMachine for SendStateMachine {
    type Outcome = SendOutcome;

    async fn trigger(&self, ctx: &ClientContext) -> Self::Outcome {
        // The broker waits for the contract itself, so the request goes
        // out before the funding transaction is accepted; acceptance is
        // only awaited for its rejection.
        tokio::select! {
            _ = await_rejected_sm(ctx, self.operation, self.outpoint.txid) => SendOutcome::Rejected,
            attestation = broker_swap_sm(
                ctx.brokers.clone(),
                self.broker,
                ctx.mint,
                self.outpoint,
                self.send.clone(),
                self.receive.clone(),
            ) => SendOutcome::Attested(attestation),
        }
    }

    fn transition(
        &self,
        ctx: &ClientContext,
        dbtx: &WriteTx,
        outcome: Self::Outcome,
    ) -> Option<Self> {
        match outcome {
            SendOutcome::Rejected => None,
            SendOutcome::Attested(attestation) => {
                ctx.log_event(
                    dbtx,
                    self.account,
                    self.operation,
                    SendSuccessEvent { attestation },
                );
                None
            }
        }
    }
}

/// Resolves only with an attestation the send contract accepts. An invalid
/// response or a broker that left the announced set is terminal for this
/// branch — no retry changes either — so it parks.
#[instrument(skip(brokers, send, receive))]
async fn broker_swap_sm(
    brokers: Brokers,
    broker: BrokerPk,
    mint: MintId,
    outpoint: OutPoint,
    send: SendContract,
    receive: ReceiveContract,
) -> Signature {
    let method = BrokerMethod::Swap(SwapRequest {
        mint,
        outpoint,
        receive,
    });

    match brokers.request_retry::<SwapResponse>(broker, method).await {
        Ok(response) => {
            if send.verify_attestation(&response.attestation) {
                return response.attestation;
            }

            warn!("Invalid broker attestation");
        }
        Err(e) => warn!(error = %e, "Broker swap failed"),
    }

    pending().await
}

/// Resolves only if the funding transaction is rejected; acceptance is
/// not an outcome of its own, the broker's attestation is.
async fn await_rejected_sm(ctx: &ClientContext, operation: OperationId, txid: TransactionId) {
    if ctx.await_tx_accepted(operation, txid).await.is_ok() {
        pending().await
    }
}
