mod api;
pub mod events;
mod receive_sm;
mod secret;

use anyhow::Context as _;
use picomint_sqlite::WriteTx;
use std::collections::BTreeMap;

use crate::executor::ModuleExecutor;
use crate::module::ClientContext;
use crate::task::TaskGroup;
use crate::tx::{Input, Output, TxBuilder};
use events::{ReceiveEvent, SendCancelEvent, SendEvent, SendSuccessEvent};
use picomint_core::config::FederationId;
use picomint_core::core::{Account, OperationId};
use picomint_core::ln::config::LightningConfigConsensus;
use picomint_core::ln::contracts::{IncomingContract, IncomingOffer, OutgoingContract};
use picomint_core::ln::{LightningInput, LightningOutput, OutgoingWitness};
use picomint_core::secp256k1::{Keypair, XOnlyPublicKey};
use picomint_core::wire;
use picomint_core::{Amount, OutPoint, PeerId, secp256k1};
use secp256k1::schnorr::Signature;
use tpe::{AggregatePublicKey, PublicKeyShare};
use tracing::warn;

pub use self::secret::GwSecret;
use receive_sm::{ReceiveStateMachine, ReceiveStateMachineTable};

/// A gateway client holds a single balance, so every account-scoped call this
/// module makes names this one. Accounts are a wallet-facing split; the
/// gateway has no use for a second balance and [`GwSecret`] grows no account
/// hop to derive one.
pub const GATEWAY_ACCOUNT: Account = Account::PRIMARY;

impl GatewayClientModule {
    pub fn new(
        federation: FederationId,
        cfg: LightningConfigConsensus,
        context: ClientContext,
        mint: crate::mint::MintClientModule,
        gw_secret: GwSecret,
        tg: &TaskGroup,
    ) -> GatewayClientModule {
        let keypair = gw_secret.contract_keypair();

        let sm_context = GwSmContext {
            client_ctx: context.clone(),
            mint: mint.clone(),
            input_fee: cfg.input_fee,
            keypair,
            tpe_agg_pk: cfg.tpe_agg_pk,
            tpe_pks: cfg.tpe_pks.clone(),
        };

        let receive_executor = ModuleExecutor::new(
            context.db().clone(),
            federation,
            ReceiveStateMachineTable,
            sm_context,
            tg.clone(),
        );

        GatewayClientModule {
            federation,
            cfg,
            client_ctx: context,
            mint,
            keypair,
            receive_executor,
        }
    }
}

#[derive(Clone)]
pub struct GatewayClientModule {
    pub federation: FederationId,
    pub cfg: LightningConfigConsensus,
    pub client_ctx: ClientContext,
    pub mint: crate::mint::MintClientModule,
    pub keypair: Keypair,
    receive_executor: ModuleExecutor<ReceiveStateMachine, ReceiveStateMachineTable>,
}

/// Context shared with the ReceiveSM executor.
#[derive(Clone)]
pub struct GwSmContext {
    pub client_ctx: ClientContext,
    pub mint: crate::mint::MintClientModule,
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
    /// public `Send` handler after it has inserted the outgoing contract row
    /// in the daemon DB. Called at most once per operation id — `AppState::send`
    /// short-circuits on the existing `OutgoingContract` row.
    ///
    /// `dbtx` must be scoped to this federation's client DB namespace (see
    /// [`::isolate`]).
    pub fn log_send_started(
        &self,
        dbtx: &WriteTx,
        operation: OperationId,
        outpoint: OutPoint,
        amount: Amount,
        fee: Amount,
    ) {
        self.client_ctx.log_event(
            dbtx,
            GATEWAY_ACCOUNT,
            operation,
            SendEvent {
                outpoint,
                amount,
                fee,
            },
        );
    }

    /// Fund an incoming offer: attach a refund key, submit the resulting
    /// contract to the federation, log `ReceiveEvent`, and spawn the
    /// `ReceiveStateMachine`. Called by the daemon's LDK `PaymentClaimable`
    /// handler (for LN receives) and by the daemon's `/send-payment`
    /// direct-swap path.
    ///
    /// The refund key is ours by construction rather than by agreement — the
    /// recipient never names it, and cannot, since the offer id does not
    /// cover it. It is fresh per contract and kept only by the state machine
    /// below, so it leaks nothing about which gateway funded what.
    ///
    /// Idempotent on `operation`: if the incoming-contract tx has already
    /// been submitted for this operation id, this is a no-op (the existing SM will
    /// drive it).
    ///
    /// `dbtx` must be scoped to this federation's client DB namespace (see
    /// [`::isolate`]).
    pub fn start_receive(
        &self,
        dbtx: &WriteTx,
        operation: OperationId,
        offer: IncomingOffer,
    ) -> anyhow::Result<()> {
        let refund_keypair = Keypair::new(secp256k1::SECP256K1, &mut rand::thread_rng());

        let contract = IncomingContract {
            offer: offer.clone(),
            refund_pk: refund_keypair.x_only_public_key().0,
        };

        let tx_builder = TxBuilder::from_output(Output {
            output: wire::Output::Ln(Box::new(LightningOutput::Incoming(contract))),
            amount: offer.commitment.amount - offer.commitment.fee,
            fee: self.cfg.output_fee,
        });

        let amount = offer.commitment.amount;
        let fee = offer.commitment.fee;

        let txid = self
            .mint
            .finalize_and_submit_tx(
                dbtx,
                GATEWAY_ACCOUNT,
                operation,
                tx_builder,
                false,
                |txid| ReceiveEvent { txid, amount, fee },
            )
            .context("Insufficient funds")?;

        let outpoint = OutPoint { txid, out_idx: 0 };

        self.receive_executor.add_state_machine_dbtx(
            dbtx,
            ReceiveStateMachine {
                operation,
                offer,
                outpoint,
                refund_keypair,
            },
        );

        Ok(())
    }

    /// Terminal work for an outgoing contract. Called by:
    ///   - the daemon's LDK `PaymentSuccessful` / `PaymentFailed` event handler
    ///     (external LN sends);
    ///   - the per-federation trailer on direct-swap receives.
    ///
    /// `Some((preimage, ln_fee))` claims the outgoing contract and logs
    /// `SendSuccessEvent` with the realized routing cost. `None` signs the
    /// forfeit message and logs `SendCancelEvent` — a payment that failed
    /// routed nothing, so there is no cost to report.
    ///
    /// Called at most once per operation id: both callers short-circuit re-entry
    /// via upstream markers (`ProcessedLdkEventTable` on the LDK path,
    /// `EventCursorTable` on the trailer path) in the same unified dbtx.
    ///
    /// `dbtx` must be scoped to this federation's client DB namespace (see
    /// [`::isolate`]).
    pub fn finalize_send(
        &self,
        dbtx: &WriteTx,
        operation: OperationId,
        contract: OutgoingContract,
        outpoint: OutPoint,
        success: Option<([u8; 32], Amount)>,
    ) {
        match success {
            Some((preimage, ln_fee)) => {
                let tx_builder = TxBuilder::from_input(Input {
                    input: wire::Input::Ln(LightningInput::Outgoing(
                        outpoint,
                        OutgoingWitness::Claim(preimage),
                    )),
                    keypair: self.keypair,
                    amount: contract.amount + contract.fee,
                    fee: self.cfg.input_fee,
                });

                self.mint
                    .finalize_and_submit_tx(
                        dbtx,
                        GATEWAY_ACCOUNT,
                        operation,
                        tx_builder,
                        false,
                        |txid| SendSuccessEvent {
                            preimage,
                            txid,
                            ln_fee,
                        },
                    )
                    .expect("Cannot claim outgoing contract — additional funding needed");
            }
            None => {
                let signature = self.keypair.sign_schnorr(contract.forfeit_message());
                self.client_ctx.log_event(
                    dbtx,
                    GATEWAY_ACCOUNT,
                    operation,
                    SendCancelEvent { signature },
                );
            }
        }
    }

    /// Subscribe to this federation's event log and await either
    /// `SendSuccessEvent` or `SendCancelEvent` for `operation`. Replays
    /// history so a completed op returns immediately.
    pub async fn subscribe_send(&self, operation: OperationId) -> Result<[u8; 32], Signature> {
        use futures::StreamExt as _;

        let mut stream = self.client_ctx.subscribe_operation_events(operation);
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

/// Remove every row this module owns under the caller's federation prefix.
/// Called by [`crate::Client::remove`] for end-of-life cleanup.
pub(crate) fn wipe_tables(dbtx: &WriteTx, federation: picomint_core::config::FederationId) {
    dbtx.remove_prefix(&ReceiveStateMachineTable, &federation);
}

// ─── Flat federation-keyed surface ───────────────────────────────────────

impl crate::Client {
    /// The public key this gateway's contracts are keyed to on `federation`.
    pub fn gw_pk(&self, federation: FederationId) -> anyhow::Result<XOnlyPublicKey> {
        Ok(self.runtime(federation)?.gw.keypair.x_only_public_key().0)
    }

    /// Log a `SendEvent` on the federation's event log. See
    /// [`GatewayClientModule::log_send_started`].
    pub fn gw_log_send_started(
        &self,
        federation: FederationId,
        dbtx: &WriteTx,
        operation: OperationId,
        outpoint: OutPoint,
        amount: Amount,
        fee: Amount,
    ) -> anyhow::Result<()> {
        self.runtime(federation)?
            .gw
            .log_send_started(dbtx, operation, outpoint, amount, fee);

        Ok(())
    }

    /// Fund an incoming offer and spawn the state machine that drives it to
    /// settlement. See [`GatewayClientModule::start_receive`].
    pub fn gw_start_receive(
        &self,
        federation: FederationId,
        dbtx: &WriteTx,
        operation: OperationId,
        offer: IncomingOffer,
    ) -> anyhow::Result<()> {
        self.runtime(federation)?
            .gw
            .start_receive(dbtx, operation, offer)
    }

    /// Settle an outgoing contract: claim it with the preimage on success,
    /// or log the forfeit signature on failure. See
    /// [`GatewayClientModule::finalize_send`].
    pub fn gw_finalize_send(
        &self,
        federation: FederationId,
        dbtx: &WriteTx,
        operation: OperationId,
        contract: OutgoingContract,
        outpoint: OutPoint,
        success: Option<([u8; 32], Amount)>,
    ) -> anyhow::Result<()> {
        self.runtime(federation)?
            .gw
            .finalize_send(dbtx, operation, contract, outpoint, success);

        Ok(())
    }

    /// Await either `SendSuccessEvent` (the preimage) or `SendCancelEvent`
    /// (the forfeit signature) for `operation`. Replays history, so a
    /// completed operation returns immediately.
    pub async fn gw_subscribe_send(
        &self,
        federation: FederationId,
        operation: OperationId,
    ) -> anyhow::Result<Result<[u8; 32], Signature>> {
        let runtime = self.runtime(federation)?;

        Ok(runtime.gw.subscribe_send(operation).await)
    }
}
