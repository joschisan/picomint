use crate::executor::{SmId, StateMachine};
use crate::tx::{Input, TxBuilder};
use anyhow::ensure;
use bitcoin::hashes::sha256;
use futures::future::pending;
use iroh::Endpoint;
use picomint_core::TransactionId;
use picomint_core::backoff::{Retryable, networking_backoff};
use picomint_core::config::FederationId;
use picomint_core::core::OperationId;
use picomint_core::ln::contracts::OutgoingContract;
use picomint_core::ln::gateway::GatewayPk;
use picomint_core::ln::{LightningInput, OutgoingWitness};
use picomint_core::wire;
use picomint_core::{OutPoint, secp256k1};
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::WriteTx;
use secp256k1::Keypair;
use secp256k1::schnorr::Signature;
use tracing::{error, instrument};

use super::events::{SendFailureEvent, SendRefundEvent, SendSuccessEvent};
use super::{LightningClientContext, LightningInvoice};

crate::client_table!(
    SendStateMachineTable,
    SmId => SendStateMachine,
    "ln-send-sm",
);

#[derive(Debug, Clone, Eq, PartialEq, Hash, Decodable, Encodable)]
pub struct SendStateMachine {
    pub common: SendSMCommon,
    pub state: SendSMState,
}

picomint_redb::consensus_value!(SendStateMachine);

impl SendStateMachine {
    pub fn update(&self, state: SendSMState) -> Self {
        Self {
            common: self.common.clone(),
            state,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Decodable, Encodable)]
pub struct SendSMCommon {
    pub operation: OperationId,
    pub outpoint: OutPoint,
    pub contract: OutgoingContract,
    pub gateway_pk: Option<GatewayPk>,
    pub invoice: Option<LightningInvoice>,
    pub refund_keypair: Keypair,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Decodable, Encodable)]
pub enum SendSMState {
    Funding,
    Funded,
    Refunding(TransactionId),
}

/// Outcome produced by [`SendStateMachine::trigger`]. Which variant is
/// yielded depends on the current [`SendSMState`]:
/// - `Funding`     → [`SendOutcome::FundingResult`]
/// - `Funded`      → [`SendOutcome::GatewayResponse`] / [`SendOutcome::PreimageTable`]
///   / [`SendOutcome::Expired`]
/// - `Refunding{}` → [`SendOutcome::Refunded`] / [`SendOutcome::PreimageTable`]
///   / [`SendOutcome::Failure`]
pub enum SendOutcome {
    FundingResult(Result<(), String>),
    GatewayResponse(Result<[u8; 32], Signature>),
    PreimageTable([u8; 32]),
    Expired,
    Refunded,
    Failure,
}

/// State machine that requests the lightning gateway to pay an invoice on
/// behalf of a federation client.
impl StateMachine for SendStateMachine {
    type Context = LightningClientContext;
    type Outcome = SendOutcome;

    async fn trigger(&self, ctx: &Self::Context) -> Self::Outcome {
        match &self.state {
            SendSMState::Funding => SendOutcome::FundingResult(
                ctx.client_ctx
                    .await_tx_accepted(self.common.operation, self.common.outpoint.txid)
                    .await,
            ),
            SendSMState::Funded => {
                let gateway_pk = self.common.gateway_pk.unwrap();
                let invoice = self.common.invoice.clone().unwrap();
                tokio::select! {
                    response = gateway_send_payment_sm(
                        ctx.client_ctx.api().endpoint().clone(),
                        gateway_pk,
                        ctx.federation,
                        self.common.outpoint,
                        self.common.contract.clone(),
                        invoice,
                        self.common.refund_keypair,
                    ) => SendOutcome::GatewayResponse(response),
                    preimage = await_preimage_sm(
                        self.common.outpoint,
                        self.common.contract.clone(),
                        ctx.clone(),
                    ) => match preimage {
                        Some(p) => SendOutcome::PreimageTable(p),
                        None => SendOutcome::Expired,
                    },
                }
            }
            SendSMState::Refunding(refund_txid) => {
                match ctx
                    .client_ctx
                    .await_tx_accepted(self.common.operation, *refund_txid)
                    .await
                {
                    Ok(()) => SendOutcome::Refunded,
                    Err(_) => {
                        // Refund tx was rejected, which means the contract input
                        // is gone — the gateway must have claimed it. Re-poll the
                        // federation for the preimage one more time before giving
                        // up.
                        let p = ctx
                            .client_ctx
                            .api()
                            .ln_await_preimage(self.common.outpoint, self.common.contract.expiry)
                            .await
                            .filter(|p| self.common.contract.verify_preimage(p));
                        match p {
                            Some(p) => SendOutcome::PreimageTable(p),
                            None => SendOutcome::Failure,
                        }
                    }
                }
            }
        }
    }

    fn transition(
        &self,
        ctx: &Self::Context,
        dbtx: &WriteTx,
        outcome: Self::Outcome,
    ) -> Option<Self> {
        match outcome {
            SendOutcome::FundingResult(Ok(())) => Some(self.update(SendSMState::Funded)),
            SendOutcome::FundingResult(Err(_)) => None,
            SendOutcome::PreimageTable(preimage) => {
                ctx.client_ctx.log_event(
                    dbtx,
                    self.common.operation,
                    SendSuccessEvent { preimage },
                );
                None
            }
            SendOutcome::GatewayResponse(Ok(preimage)) => {
                ctx.client_ctx.log_event(
                    dbtx,
                    self.common.operation,
                    SendSuccessEvent { preimage },
                );
                None
            }
            SendOutcome::GatewayResponse(Err(signature)) => {
                Some(self.update(SendSMState::Refunding(submit_refund(
                    ctx,
                    dbtx,
                    self,
                    OutgoingWitness::Cancel(signature),
                    false,
                ))))
            }
            SendOutcome::Expired => Some(self.update(SendSMState::Refunding(submit_refund(
                ctx,
                dbtx,
                self,
                OutgoingWitness::Refund,
                true,
            )))),
            SendOutcome::Refunded => None,
            SendOutcome::Failure => {
                ctx.client_ctx
                    .log_event(dbtx, self.common.operation, SendFailureEvent);
                None
            }
        }
    }
}

/// Build and submit the refund-claim tx, log `SendRefundEvent`, return its
/// txid for the SM to advance into the `Refunding` state with.
fn submit_refund(
    ctx: &LightningClientContext,
    dbtx: &WriteTx,
    old_state: &SendStateMachine,
    witness: OutgoingWitness,
    expired: bool,
) -> TransactionId {
    let tx_builder = TxBuilder::from_input(Input {
        input: wire::Input::Ln(LightningInput::Outgoing(old_state.common.outpoint, witness)),
        keypair: old_state.common.refund_keypair,
        amount: old_state.common.contract.amount + old_state.common.contract.fee,
        fee: ctx.input_fee,
    });

    let operation = old_state.common.operation;

    ctx.mint
        .finalize_and_submit_tx(dbtx, operation, tx_builder, |txid| SendRefundEvent {
            txid,
            expired,
        })
        .expect("Cannot claim input, additional funding needed")
}

#[instrument(skip(refund_keypair, endpoint))]
async fn gateway_send_payment_sm(
    endpoint: Endpoint,
    gateway_pk: GatewayPk,
    federation: FederationId,
    outpoint: OutPoint,
    contract: OutgoingContract,
    invoice: LightningInvoice,
    refund_keypair: Keypair,
) -> Result<[u8; 32], Signature> {
    (|| async {
        let payment_result = crate::ln::gateway::send_payment(
            &endpoint,
            gateway_pk,
            federation,
            outpoint,
            contract.clone(),
            invoice.clone(),
            refund_keypair.sign_schnorr(secp256k1::Message::from_digest(
                *invoice.consensus_hash::<sha256::Hash>().as_ref(),
            )),
        )
        .await?;

        ensure!(
            contract.verify_gateway_response(&payment_result),
            "Invalid gateway response: {payment_result:?}"
        );

        Ok(payment_result)
    })
    .retry(networking_backoff())
    .await
    .expect("networking_backoff retries forever")
}

#[instrument(skip(ctx))]
async fn await_preimage_sm(
    outpoint: OutPoint,
    contract: OutgoingContract,
    ctx: LightningClientContext,
) -> Option<[u8; 32]> {
    let preimage = ctx
        .client_ctx
        .api()
        .ln_await_preimage(outpoint, contract.expiry)
        .await?;

    if contract.verify_preimage(&preimage) {
        return Some(preimage);
    }

    error!("Federation returned invalid preimage {:?}", preimage);

    pending().await
}
