mod api;
pub mod events;
mod receive_sm;
mod secret;

use anyhow::Context as _;
use picomint_sqlite::WriteTx;
use std::collections::BTreeMap;

use crate::client::{Client, FederationRuntime};
use crate::executor::ModuleExecutor;
use crate::mint::Mint;
use crate::module::ClientContext;
use crate::secret::ClientSecret;
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

impl Gw {
    pub fn new(
        federation: FederationId,
        cfg: LightningConfigConsensus,
        context: ClientContext,
        mint: Mint,
        gw_secret: GwSecret,
        tg: &TaskGroup,
    ) -> Gw {
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

        Gw {
            federation,
            cfg,
            client_ctx: context,
            mint,
            keypair,
            receive_executor,
        }
    }

    /// Derive the gateway record for `federation` — a throwaway bundle of
    /// config, keys and executor handles, rebuilt per call from the seed
    /// and the runtime's config.
    pub(crate) fn derive(
        client: &Client,
        runtime: &FederationRuntime,
        federation: FederationId,
    ) -> Gw {
        Gw::new(
            federation,
            runtime.config.ln.clone(),
            ClientContext::new(
                runtime.api.clone(),
                client.db.clone(),
                runtime.config.clone(),
            ),
            Mint::derive(client, runtime, federation),
            ClientSecret::new(&client.mnemonic, federation).gw_secret(),
            &runtime.tg,
        )
    }

    /// Resume this federation's persisted receive state machines. Called
    /// exactly once, at federation bring-up.
    pub(crate) fn resume(&self) {
        self.receive_executor.resume();
    }
}

#[derive(Clone)]
pub struct Gw {
    pub federation: FederationId,
    pub cfg: LightningConfigConsensus,
    pub client_ctx: ClientContext,
    pub mint: Mint,
    pub keypair: Keypair,
    receive_executor: ModuleExecutor<ReceiveStateMachine, ReceiveStateMachineTable>,
}

/// Context shared with the ReceiveSM executor.
#[derive(Clone)]
pub struct GwSmContext {
    pub client_ctx: ClientContext,
    pub mint: Mint,
    pub input_fee: Amount,
    pub keypair: Keypair,
    pub tpe_agg_pk: AggregatePublicKey,
    pub tpe_pks: BTreeMap<PeerId, PublicKeyShare>,
}

impl Gw {}

/// Remove every row this module owns under the caller's federation prefix.
/// Called by [`crate::Client::remove`] for end-of-life cleanup.
pub(crate) fn wipe_tables(dbtx: &WriteTx, federation: FederationId) {
    dbtx.remove_prefix(&ReceiveStateMachineTable, &federation);
}

// ─── Flat federation-keyed surface ───────────────────────────────────────

impl Client {
    /// Derive the gateway record for `federation`, bringing the federation
    /// up.
    pub(crate) fn gw(&self, federation: FederationId) -> anyhow::Result<Gw> {
        Ok(Gw::derive(self, &*self.runtime(federation)?, federation))
    }

    /// The public key this gateway's contracts are keyed to on `federation`.
    pub fn gw_pk(&self, federation: FederationId) -> anyhow::Result<XOnlyPublicKey> {
        Ok(self.gw(federation)?.keypair.x_only_public_key().0)
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
        let gw = self.gw(federation)?;

        gw.client_ctx.log_event(
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
        let gw = self.gw(federation)?;

        let refund_keypair = Keypair::new(secp256k1::SECP256K1, &mut rand::thread_rng());

        let contract = IncomingContract {
            offer: offer.clone(),
            refund_pk: refund_keypair.x_only_public_key().0,
        };

        let tx_builder = TxBuilder::from_output(Output {
            output: wire::Output::Ln(Box::new(LightningOutput::Incoming(contract))),
            amount: offer.commitment.amount - offer.commitment.fee,
            fee: gw.cfg.output_fee,
        });

        let amount = offer.commitment.amount;
        let fee = offer.commitment.fee;

        let txid = gw
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

        gw.receive_executor.add_state_machine_dbtx(
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
        let gw = self.gw(federation)?;

        match success {
            Some((preimage, ln_fee)) => {
                let tx_builder = TxBuilder::from_input(Input {
                    input: wire::Input::Ln(LightningInput::Outgoing(
                        outpoint,
                        OutgoingWitness::Claim(preimage),
                    )),
                    keypair: gw.keypair,
                    amount: contract.amount + contract.fee,
                    fee: gw.cfg.input_fee,
                });

                gw.mint
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
                let signature = gw.keypair.sign_schnorr(contract.forfeit_message());
                gw.client_ctx.log_event(
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
        let gw = self.gw(federation)?;

        use futures::StreamExt as _;

        let mut stream = gw.client_ctx.subscribe_operation_events(operation);
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
