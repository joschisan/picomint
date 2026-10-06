pub mod api;
pub mod events;
mod secret;

use anyhow::{Context as _, ensure};
use picomint_redb::WriteTx;

use crate::client::Client;
use crate::tx::{Input, Output, TxBuilder};
use events::{ReceiveEvent, SendCancelEvent, SendEvent, SendSuccessEvent};
use picomint_core::config::MintId;
use picomint_core::core::{Account, OperationId};
use picomint_core::lightning::contracts::{IncomingContract, OutgoingContract, forfeit_message};
use picomint_core::lightning::{
    LIGHTNING_INPUT_FEE, LIGHTNING_OUTPUT_FEE, LightningInput, LightningOutput, OutgoingWitness,
};
use picomint_core::secp256k1::XOnlyPublicKey;
use picomint_core::wire;
use picomint_core::{Amount, OutPoint};
use secp256k1::schnorr::Signature;
use tracing::{error, warn};

pub use self::secret::GatewaySecret;

/// The account payments are routed from: every contract this module funds
/// or claims settles here. The other accounts are the operator's — funds
/// parked there through the admin CLI are outside the routing pool.
pub const ROUTING_ACCOUNT: Account = Account::Primary;

// ─── Flat mint-keyed surface ───────────────────────────────────────

impl Client {
    /// The public key this gateway's contracts are keyed to on `mint`.
    pub fn gateway_pk(&self, mint: MintId) -> anyhow::Result<XOnlyPublicKey> {
        let ctx = self.ctx(mint)?;

        Ok(ctx
            .secret
            .gateway_secret()
            .contract_keypair()
            .x_only_public_key()
            .0)
    }

    /// Log a `SendEvent` on the mint's event log. Called by the
    /// daemon's public `Send` handler after it has inserted the outgoing
    /// contract row in the daemon DB; at most once per operation id.
    pub fn gateway_log_send_started(
        &self,
        mint: MintId,
        dbtx: &WriteTx,
        operation: OperationId,
        outpoint: OutPoint,
        amount: Amount,
        fee: Amount,
    ) -> anyhow::Result<()> {
        ensure!(self.is_added(mint), "Mint is not added");

        crate::eventlog::log_event(
            dbtx,
            mint,
            ROUTING_ACCOUNT,
            operation,
            SendEvent {
                outpoint,
                amount,
                fee,
            },
        );

        Ok(())
    }

    /// Fund an incoming contract: submit it and log `ReceiveEvent`. Each
    /// call funds the contract anew, so the caller funds a contract once.
    pub fn gateway_start_receive(
        &self,
        mint: MintId,
        dbtx: &WriteTx,
        operation: OperationId,
        contract: IncomingContract,
    ) -> anyhow::Result<()> {
        let ctx = self.ctx(mint)?;

        let amount = contract.amount;
        let fee = contract.fee;

        let claim_amount = contract
            .claim_amount()
            .expect("The receive handler refuses a contract whose fee exceeds its amount");

        let tx_builder = TxBuilder::from_output(Output {
            output: wire::Output::Lightning(Box::new(LightningOutput::Incoming(contract))),
            amount: claim_amount,
            fee: LIGHTNING_OUTPUT_FEE,
        });

        crate::ecash::finalize_and_submit_tx(
            &ctx,
            dbtx,
            ROUTING_ACCOUNT,
            operation,
            tx_builder,
            Vec::new(),
            false,
            |txid| ReceiveEvent { txid, amount, fee },
        )
        .context("Insufficient funds")
        .map(|_| ())
    }

    /// Settle an outgoing contract: claim it with the preimage on success,
    /// or log the forfeit signature on failure. Idempotent via the caller's
    /// upstream markers, committed in the same dbtx.
    pub fn gateway_finalize_send(
        &self,
        mint: MintId,
        dbtx: &WriteTx,
        operation: OperationId,
        contract: OutgoingContract,
        outpoint: OutPoint,
        success: Option<([u8; 32], Amount)>,
    ) -> anyhow::Result<()> {
        let ctx = self.ctx(mint)?;

        match success {
            Some((preimage, lightning_fee)) => {
                let tx_builder = TxBuilder::from_input(Input {
                    input: wire::Input::Lightning(LightningInput::Outgoing(
                        outpoint,
                        OutgoingWitness::Claim(preimage),
                    )),
                    keypair: ctx.secret.gateway_secret().contract_keypair(),
                    amount: contract.amount + contract.fee,
                    fee: LIGHTNING_INPUT_FEE,
                });

                let claimed = crate::ecash::finalize_and_submit_tx(
                    &ctx,
                    dbtx,
                    ROUTING_ACCOUNT,
                    operation,
                    tx_builder,
                    Vec::new(),
                    false,
                    |txid| SendSuccessEvent {
                        preimage,
                        txid,
                        lightning_fee,
                    },
                );

                if claimed.is_none() {
                    error!(%operation, "The outgoing contract is too small to claim");
                }
            }
            None => {
                let signature = ctx
                    .secret
                    .gateway_secret()
                    .contract_keypair()
                    .sign_schnorr(forfeit_message(contract.contract_id()));
                ctx.log_event(
                    dbtx,
                    ROUTING_ACCOUNT,
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
    pub async fn gateway_subscribe_send(
        &self,
        mint: MintId,
        operation: OperationId,
    ) -> anyhow::Result<Result<[u8; 32], Signature>> {
        let ctx = self.ctx(mint)?;

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
