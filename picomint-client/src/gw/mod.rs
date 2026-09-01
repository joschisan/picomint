mod api;
pub mod events;
mod receive_sm;
mod secret;

use anyhow::Context as _;
use picomint_redb::{Database, DbRead, ReadTx, WriteTx};
use std::sync::Arc;
use tokio::sync::Notify;

use crate::client::Client;
use crate::module::ClientContext;
use crate::tx::{Input, Output, TxBuilder};
use events::{ReceiveEvent, SendCancelEvent, SendEvent, SendSuccessEvent};
use picomint_core::config::FederationId;
use picomint_core::core::{Account, OperationId};
use picomint_core::ln::contracts::{IncomingContract, IncomingOffer, OutgoingContract};
use picomint_core::ln::{LightningInput, LightningOutput, OutgoingWitness};
use picomint_core::secp256k1::{Keypair, XOnlyPublicKey};
use picomint_core::wire;
use picomint_core::{Amount, OutPoint, secp256k1};
use secp256k1::schnorr::Signature;
use tracing::warn;

pub use self::secret::GwSecret;
use receive_sm::{ReceiveStateMachine, ReceiveStateMachineTable};

/// A gateway client holds a single balance, so every account-scoped call this
/// module makes names this one. Accounts are a wallet-facing split; the
/// gateway has no use for a second balance and [`GwSecret`] grows no account
/// hop to derive one.
pub const GATEWAY_ACCOUNT: Account = Account::PRIMARY;

/// Resume this federation's persisted receive state machines. Called
/// exactly once, at federation bring-up.
pub(crate) fn resume(ctx: &ClientContext) {
    crate::executor::resume::<ReceiveStateMachine, _>(ctx, ReceiveStateMachineTable);
}

/// Remove every row this module owns under the caller's federation prefix.
/// Called by [`crate::Client::remove`] for end-of-life cleanup.
pub(crate) fn wipe_tables(dbtx: &WriteTx, federation: FederationId) {
    dbtx.remove_prefix(&ReceiveStateMachineTable, &federation);
}

/// Whether any of this module's state machines for `operation` is still
/// active under `federation`.
pub(crate) fn operation_is_active(
    dbtx: &ReadTx,
    federation: FederationId,
    operation: OperationId,
) -> bool {
    dbtx.prefix(&ReceiveStateMachineTable, &federation, |r| {
        r.any(|entry| entry.1.operation == operation)
    })
}

/// Notify handles for this module's state machine tables, fired on every
/// commit that writes them.
pub(crate) fn sm_notifies(db: &Database) -> Vec<Arc<Notify>> {
    vec![db.notify_for_table(&ReceiveStateMachineTable)]
}

// ─── Flat federation-keyed surface ───────────────────────────────────────

impl Client {
    /// The public key this gateway's contracts are keyed to on `federation`.
    pub fn gw_pk(&self, federation: FederationId) -> anyhow::Result<XOnlyPublicKey> {
        let ctx = self.ctx(federation)?;

        Ok(ctx
            .secret
            .gw_secret()
            .contract_keypair()
            .x_only_public_key()
            .0)
    }

    /// Log a `SendEvent` on the federation's event log. Called by the
    /// daemon's public `Send` handler after it has inserted the outgoing
    /// contract row in the daemon DB; at most once per operation id.
    pub fn gw_log_send_started(
        &self,
        federation: FederationId,
        dbtx: &WriteTx,
        operation: OperationId,
        outpoint: OutPoint,
        amount: Amount,
        fee: Amount,
    ) -> anyhow::Result<()> {
        let ctx = self.ctx(federation)?;

        ctx.log_event(
            dbtx,
            GATEWAY_ACCOUNT,
            operation,
            SendEvent {
                outpoint,
                amount,
                fee,
            },
        );

        Ok(())
    }

    /// Fund an incoming offer: attach a fresh refund key, submit the
    /// resulting contract, log `ReceiveEvent`, and spawn the state machine
    /// that drives it to settlement. Idempotent on `operation`.
    pub fn gw_start_receive(
        &self,
        federation: FederationId,
        dbtx: &WriteTx,
        operation: OperationId,
        offer: IncomingOffer,
    ) -> anyhow::Result<()> {
        let ctx = self.ctx(federation)?;

        let refund_keypair = Keypair::new(secp256k1::SECP256K1, &mut rand::thread_rng());

        let contract = IncomingContract {
            offer: offer.clone(),
            refund_pk: refund_keypair.x_only_public_key().0,
        };

        let tx_builder = TxBuilder::from_output(Output {
            output: wire::Output::Ln(Box::new(LightningOutput::Incoming(contract))),
            amount: offer.commitment.amount - offer.commitment.fee,
            fee: ctx.config.ln.output_fee,
        });

        let amount = offer.commitment.amount;
        let fee = offer.commitment.fee;

        let txid = crate::mint::finalize_and_submit_tx(
            &ctx,
            dbtx,
            GATEWAY_ACCOUNT,
            operation,
            tx_builder,
            false,
            |txid| ReceiveEvent { txid, amount, fee },
        )
        .context("Insufficient funds")?;

        let outpoint = OutPoint { txid, out_idx: 0 };

        crate::executor::add_state_machine_dbtx(
            &ctx,
            ReceiveStateMachineTable,
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

    /// Settle an outgoing contract: claim it with the preimage on success,
    /// or log the forfeit signature on failure. Idempotent via the caller's
    /// upstream markers, committed in the same dbtx.
    pub fn gw_finalize_send(
        &self,
        federation: FederationId,
        dbtx: &WriteTx,
        operation: OperationId,
        contract: OutgoingContract,
        outpoint: OutPoint,
        success: Option<([u8; 32], Amount)>,
    ) -> anyhow::Result<()> {
        let ctx = self.ctx(federation)?;

        match success {
            Some((preimage, ln_fee)) => {
                let tx_builder = TxBuilder::from_input(Input {
                    input: wire::Input::Ln(LightningInput::Outgoing(
                        outpoint,
                        OutgoingWitness::Claim(preimage),
                    )),
                    keypair: ctx.secret.gw_secret().contract_keypair(),
                    amount: contract.amount + contract.fee,
                    fee: ctx.config.ln.input_fee,
                });

                crate::mint::finalize_and_submit_tx(
                    &ctx,
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
                let signature = ctx
                    .secret
                    .gw_secret()
                    .contract_keypair()
                    .sign_schnorr(contract.forfeit_message());
                ctx.log_event(
                    dbtx,
                    GATEWAY_ACCOUNT,
                    operation,
                    SendCancelEvent { signature },
                );
            }
        }

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
        let ctx = self.ctx(federation)?;

        use futures::StreamExt as _;

        let mut stream = ctx.subscribe_operation_events(operation);
        while let Some(entry) = stream.next().await {
            if let Some(ev) = entry.to_event::<SendSuccessEvent>() {
                return Ok(Ok(ev.preimage));
            }
            if let Some(ev) = entry.to_event::<SendCancelEvent>() {
                warn!("Outgoing lightning payment is cancelled");
                return Ok(Err(ev.signature));
            }
        }
        unreachable!("subscribe_operation_events only ends at client shutdown")
    }
}
