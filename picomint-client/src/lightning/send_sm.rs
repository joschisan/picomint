use super::gateway::Gateways;
use crate::api::MintApi;
use crate::executor::{SmId, StateMachine};
use crate::tx::{Input, TxBuilder};
use bitcoin::hashes::sha256;
use futures::future::pending;
use picomint_core::TransactionId;
use picomint_core::config::MintId;
use picomint_core::core::{Account, OperationId};
use picomint_core::lightning::contracts::OutgoingContract;
use picomint_core::lightning::gateway::GatewayPk;
use picomint_core::lightning::{LightningInput, OutgoingWitness};
use picomint_core::wire;
use picomint_core::{OutPoint, secp256k1};
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::{WriteTx, table};
use secp256k1::Keypair;
use secp256k1::schnorr::Signature;
use tracing::{error, instrument, warn};

use super::LightningInvoice;
use super::events::{SendRefundEvent, SendSuccessEvent};
use crate::context::ClientContext;

table!(
    SendStateMachineTable,
    (MintId, SmId) => SendStateMachine,
    "lightning-send-sm",
);

#[derive(Debug, Clone, Eq, PartialEq, Hash, Decodable, Encodable)]
pub struct SendStateMachine {
    pub common: SendSMCommon,
    pub state: SendSMState,
}

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
    /// Account that funded the contract, and the one a refund returns to.
    pub account: Account,
    pub operation: OperationId,
    pub outpoint: OutPoint,
    pub contract: OutgoingContract,
    pub gateway_pk: GatewayPk,
    pub invoice: LightningInvoice,
    pub refund_keypair: Keypair,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Decodable, Encodable)]
pub enum SendSMState {
    Funding,
    Refunding(TransactionId),
}

/// Outcome produced by [`SendStateMachine::trigger`]. Which variant is
/// yielded depends on the current [`SendSMState`]:
/// - `Funding`     → [`SendOutcome::Rejected`] / [`SendOutcome::GatewayResponse`]
///   / [`SendOutcome::PreimageTable`]
/// - `Refunding{}` → [`SendOutcome::Refunded`] / [`SendOutcome::PreimageTable`]
pub enum SendOutcome {
    Rejected,
    GatewayResponse(Result<[u8; 32], Signature>),
    PreimageTable([u8; 32]),
    Refunded,
}

/// State machine that requests the lightning gateway to pay an invoice on
/// behalf of a mint client.
impl StateMachine for SendStateMachine {
    type Outcome = SendOutcome;

    async fn trigger(&self, ctx: &ClientContext) -> Self::Outcome {
        match &self.state {
            SendSMState::Funding => {
                // The gateway and the mint both wait for the contract
                // themselves, so the request and the preimage poll go out
                // before the funding transaction is accepted; acceptance
                // is only awaited for its rejection. The contract settles
                // only through the gateway, by preimage or by forfeit
                // signature, so both branches wait for as long as that
                // takes.
                tokio::select! {
                    _ = await_rejected_sm(
                        ctx,
                        self.common.operation,
                        self.common.outpoint.txid,
                    ) => SendOutcome::Rejected,
                    response = gateway_send_sm(
                        ctx.gateways.clone(),
                        self.common.gateway_pk,
                        ctx.mint,
                        self.common.outpoint,
                        self.common.contract.clone(),
                        self.common.invoice.clone(),
                        self.common.refund_keypair,
                    ) => SendOutcome::GatewayResponse(response),
                    preimage = await_preimage_sm(
                        self.common.outpoint,
                        self.common.contract.clone(),
                        ctx.api.clone(),
                    ) => SendOutcome::PreimageTable(preimage),
                }
            }
            SendSMState::Refunding(refund_txid) => {
                match ctx
                    .await_tx_accepted(self.common.operation, *refund_txid)
                    .await
                {
                    Ok(()) => SendOutcome::Refunded,
                    // The refund was rejected, so the contract is gone and
                    // the only other way it goes is a claim: the preimage
                    // is in the mint's table.
                    Err(_) => SendOutcome::PreimageTable(
                        await_preimage_sm(
                            self.common.outpoint,
                            self.common.contract.clone(),
                            ctx.api.clone(),
                        )
                        .await,
                    ),
                }
            }
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
            SendOutcome::PreimageTable(preimage) => {
                ctx.log_event(
                    dbtx,
                    self.common.account,
                    self.common.operation,
                    SendSuccessEvent { preimage },
                );
                None
            }
            SendOutcome::GatewayResponse(Ok(preimage)) => {
                ctx.log_event(
                    dbtx,
                    self.common.account,
                    self.common.operation,
                    SendSuccessEvent { preimage },
                );
                None
            }
            SendOutcome::GatewayResponse(Err(signature)) => Some(self.update(
                SendSMState::Refunding(submit_refund(ctx, dbtx, self, signature)),
            )),
            SendOutcome::Refunded => None,
        }
    }
}

/// Build and submit the refund tx spending the contract with the gateway's
/// forfeit signature, log `SendRefundEvent`, return its txid for the SM to
/// advance into the `Refunding` state with.
fn submit_refund(
    ctx: &ClientContext,
    dbtx: &WriteTx,
    old_state: &SendStateMachine,
    signature: Signature,
) -> TransactionId {
    let tx_builder = TxBuilder::from_input(Input {
        input: wire::Input::Lightning(LightningInput::Outgoing(
            old_state.common.outpoint,
            OutgoingWitness::Cancel(signature),
        )),
        keypair: old_state.common.refund_keypair,
        amount: old_state.common.contract.amount + old_state.common.contract.fee,
        fee: ctx.config.lightning.input_fee,
    });

    let operation = old_state.common.operation;

    crate::ecash::finalize_and_submit_tx(
        ctx,
        dbtx,
        old_state.common.account,
        operation,
        tx_builder,
        Vec::new(),
        false,
        |txid| SendRefundEvent { txid },
    )
    .expect("Cannot claim input, additional funding needed")
}

/// Resolves only with a response the contract accepts. An invalid
/// response or a gateway that left the announced set is terminal for this
/// branch — no retry changes either — so it parks and leaves the outcome
/// to the preimage poll.
#[instrument(skip(refund_keypair, gateways))]
async fn gateway_send_sm(
    gateways: Gateways,
    gateway_pk: GatewayPk,
    mint: MintId,
    outpoint: OutPoint,
    contract: OutgoingContract,
    invoice: LightningInvoice,
    refund_keypair: Keypair,
) -> Result<[u8; 32], Signature> {
    let auth = refund_keypair.sign_schnorr(secp256k1::Message::from_digest(
        *invoice.consensus_hash::<sha256::Hash>().as_ref(),
    ));

    match gateways
        .send(gateway_pk, mint, outpoint, contract.clone(), invoice, auth)
        .await
    {
        Ok(result) => {
            if contract.verify_gateway_response(&result) {
                return result;
            }

            warn!(?result, "Invalid gateway response");
        }
        Err(e) => warn!(error = %e, "Gateway send failed"),
    }

    pending().await
}

/// Resolves only if the funding transaction is rejected; acceptance is
/// not an outcome of its own, the gateway's answer or the preimage is.
async fn await_rejected_sm(ctx: &ClientContext, operation: OperationId, txid: TransactionId) {
    if ctx.await_tx_accepted(operation, txid).await.is_ok() {
        pending().await
    }
}

#[instrument(skip(api))]
async fn await_preimage_sm(
    outpoint: OutPoint,
    contract: OutgoingContract,
    api: MintApi,
) -> [u8; 32] {
    let preimage = super::api::await_preimage(&api, outpoint).await;

    if contract.verify_preimage(&preimage) {
        return preimage;
    }

    error!("Mint returned invalid preimage {:?}", preimage);

    pending().await
}
