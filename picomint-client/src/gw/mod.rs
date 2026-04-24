mod api;
pub mod events;
mod receive_sm;
mod secret;

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::executor::ModuleExecutor;
use crate::module::ClientContext;
use crate::transaction::{Input, Output, TransactionBuilder};
use events::{ReceiveEvent, SendCancelEvent, SendEvent, SendSuccessEvent};
use picomint_core::config::FederationId;
use picomint_core::core::OperationId;
use picomint_core::ln::config::LightningConfigConsensus;
use picomint_core::ln::contracts::{IncomingContract, OutgoingContract};
use picomint_core::ln::{LightningInput, LightningInvoice, LightningOutput, OutgoingWitness};
use picomint_core::secp256k1::Keypair;
use picomint_core::task::TaskGroup;
use picomint_core::wire;
use picomint_core::{Amount, OutPoint, PeerId, secp256k1};
use picomint_redb::WriteTxRef;
use secp256k1::schnorr::Signature;
use tpe::{AggregatePublicKey, PublicKeyShare};
use tracing::warn;

pub use self::secret::GwSecret;
use receive_sm::ReceiveStateMachine;

/// Lightning CLTV Delta in blocks
pub const EXPIRATION_DELTA_MINIMUM: u64 = 144;

impl GatewayClientModule {
    pub async fn new(
        federation_id: FederationId,
        cfg: LightningConfigConsensus,
        context: ClientContext,
        mint: Arc<crate::mint::MintClientModule>,
        gw_secret: GwSecret,
        task_group: &TaskGroup,
    ) -> anyhow::Result<GatewayClientModule> {
        let keypair = gw_secret.contract_keypair();

        let sm_context = GwSmContext {
            client_ctx: context.clone(),
            mint: mint.clone(),
            input_fee: cfg.input_fee,
            keypair,
            tpe_agg_pk: cfg.tpe_agg_pk,
            tpe_pks: cfg.tpe_pks.clone(),
        };

        let receive_executor =
            ModuleExecutor::new(context.db().clone(), sm_context, task_group.clone()).await;

        Ok(GatewayClientModule {
            federation_id,
            cfg,
            client_ctx: context,
            mint,
            keypair,
            receive_executor,
        })
    }
}

#[derive(Clone)]
pub struct GatewayClientModule {
    pub federation_id: FederationId,
    pub cfg: LightningConfigConsensus,
    pub client_ctx: ClientContext,
    pub mint: Arc<crate::mint::MintClientModule>,
    pub keypair: Keypair,
    receive_executor: ModuleExecutor<ReceiveStateMachine>,
}

/// Context shared with the ReceiveSM executor.
#[derive(Clone)]
pub struct GwSmContext {
    pub client_ctx: ClientContext,
    pub mint: Arc<crate::mint::MintClientModule>,
    pub input_fee: Amount,
    pub keypair: Keypair,
    pub tpe_agg_pk: AggregatePublicKey,
    pub tpe_pks: BTreeMap<PeerId, PublicKeyShare>,
}

impl GatewayClientModule {
    pub fn input_fee(&self) -> Amount {
        self.cfg.input_fee
    }

    pub fn output_fee(&self) -> Amount {
        self.cfg.output_fee
    }

    /// Log a `SendEvent` on this federation's event log. Called by the daemon's
    /// HTTP `/send-payment` handler after it has inserted the outgoing contract
    /// row in the daemon DB. Called at most once per op id — `send_payment`
    /// short-circuits on the existing `OUTGOING_CONTRACT` row.
    ///
    /// `dbtx` must be scoped to this federation's client DB namespace (see
    /// [`WriteTxRef::isolate`]).
    pub fn log_send_started(
        &self,
        dbtx: &WriteTxRef<'_>,
        operation_id: OperationId,
        outpoint: OutPoint,
        invoice: LightningInvoice,
    ) {
        self.client_ctx
            .log_event(dbtx, operation_id, SendEvent { outpoint, invoice });
    }

    /// Bootstrap a receive: submit the IncomingContract tx to the federation,
    /// log `ReceiveEvent`, and spawn the `ReceiveStateMachine`. Called by the
    /// daemon's LDK `PaymentClaimable` handler (for LN receives) and by the
    /// daemon's `/send-payment` direct-swap path.
    ///
    /// Idempotent on `operation_id`: if the incoming-contract tx has already
    /// been submitted for this op id, this is a no-op (the existing SM will
    /// drive it).
    ///
    /// `dbtx` must be scoped to this federation's client DB namespace (see
    /// [`WriteTxRef::isolate`]).
    pub fn start_receive(
        &self,
        dbtx: &WriteTxRef<'_>,
        operation_id: OperationId,
        contract: IncomingContract,
    ) -> anyhow::Result<()> {
        let tx_builder = TransactionBuilder::from_output(Output {
            output: wire::Output::Ln(Box::new(LightningOutput::Incoming(contract.clone()))),
            amount: contract.commitment.amount,
            fee: self.cfg.output_fee,
        });

        // Idempotency: finalize_and_submit_transaction fails if a tx was
        // already submitted for this op_id. In that case the existing SM is
        // already driving the flow — nothing more to do.
        let txid = match self
            .mint
            .finalize_and_submit_transaction(dbtx, operation_id, tx_builder)
        {
            Ok(txid) => txid,
            Err(_) => return Ok(()),
        };

        let outpoint = OutPoint { txid, out_idx: 0 };

        self.receive_executor.add_state_machine_dbtx(
            dbtx,
            ReceiveStateMachine {
                operation_id,
                contract: contract.clone(),
                outpoint,
                refund_keypair: self.keypair,
            },
        );

        self.client_ctx.log_event(
            dbtx,
            operation_id,
            ReceiveEvent {
                txid: outpoint.txid,
                amount: contract.commitment.amount,
            },
        );
        Ok(())
    }

    /// Terminal work for an outgoing contract. Called by:
    ///   - the daemon's LDK `PaymentSuccessful` / `PaymentFailed` event handler
    ///     (external LN sends);
    ///   - the per-federation trailer on direct-swap receives.
    ///
    /// `Some(preimage)` claims the outgoing contract and logs
    /// `SendSuccessEvent`. `None` signs the forfeit message and logs
    /// `SendCancelEvent`.
    ///
    /// Called at most once per op id: both callers short-circuit re-entry
    /// via upstream markers (`PROCESSED_LDK_PAYMENT` on the LDK path,
    /// `TRAILER_CURSOR` on the trailer path) in the same unified dbtx.
    ///
    /// `dbtx` must be scoped to this federation's client DB namespace (see
    /// [`WriteTxRef::isolate`]).
    pub fn finalize_send(
        &self,
        dbtx: &WriteTxRef<'_>,
        operation_id: OperationId,
        contract: OutgoingContract,
        outpoint: OutPoint,
        preimage: Option<[u8; 32]>,
    ) {
        match preimage {
            Some(preimage) => {
                let tx_builder = TransactionBuilder::from_input(Input {
                    input: wire::Input::Ln(LightningInput::Outgoing(
                        outpoint,
                        OutgoingWitness::Claim(preimage),
                    )),
                    keypair: self.keypair,
                    amount: contract.amount,
                    fee: self.cfg.input_fee,
                });

                let txid = self
                    .mint
                    .finalize_and_submit_transaction(dbtx, operation_id, tx_builder)
                    .expect("Cannot claim outgoing contract — additional funding needed");

                self.client_ctx
                    .log_event(dbtx, operation_id, SendSuccessEvent { preimage, txid });
            }
            None => {
                let signature = self.keypair.sign_schnorr(contract.forfeit_message());
                self.client_ctx
                    .log_event(dbtx, operation_id, SendCancelEvent { signature });
            }
        }
    }

    /// Subscribe to this federation's event log and await either
    /// `SendSuccessEvent` or `SendCancelEvent` for `operation_id`. Replays
    /// history so a completed op returns immediately.
    pub async fn subscribe_send(&self, operation_id: OperationId) -> Result<[u8; 32], Signature> {
        use futures::StreamExt as _;

        let mut stream = self.client_ctx.subscribe_operation_events(operation_id);
        while let Some(entry) = stream.next().await {
            if let Some(ev) = entry.to_event::<SendSuccessEvent>() {
                return Ok(ev.preimage);
            }
            if let Some(ev) = entry.to_event::<SendCancelEvent>() {
                warn!("Outgoing lightning payment is cancelled");
                return Err(ev.signature);
            }
        }
        unreachable!("subscribe_operation_events only ends at client shutdown")
    }
}
