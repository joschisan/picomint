//! The broker's side of the module: swaps funded in the destination mint
//! and claimed in the source, mounted by the broker daemon through
//! [`crate::Client::new_broker`].

use anyhow::Context as _;
use futures::StreamExt as _;
use picomint_core::config::MintId;
use picomint_core::core::OperationId;
use picomint_core::secp256k1::XOnlyPublicKey;
use picomint_core::swap::methods::SwapRequest;
use picomint_core::swap::{SendContract, SwapOutput};
use picomint_core::wire;
use picomint_redb::WriteTx;
use tbs::{AggregatePublicKey, Signature};

use super::broker_sm::{BrokerStateMachine, BrokerStateMachineTable};
use super::events::{BrokerFailureEvent, BrokerSuccessEvent, BrokerSwapEvent};
use crate::client::Client;
use crate::gateway::ROUTING_ACCOUNT;
use crate::tx::{Output, TxBuilder};

impl Client {
    /// The public key this broker's send contracts are locked to on `mint`.
    pub fn swap_broker_pk(&self, mint: MintId) -> anyhow::Result<XOnlyPublicKey> {
        let ctx = self.ctx(mint)?;

        Ok(ctx
            .secret
            .swap_secret()
            .claim_keypair()
            .x_only_public_key()
            .0)
    }

    /// The added mint whose attestation key is `agg_pk`, if any: where a
    /// send contract naming that key wants its receive contract funded.
    pub fn swap_broker_destination(&self, agg_pk: AggregatePublicKey) -> Option<MintId> {
        self.mint_configs()
            .into_iter()
            .find(|entry| entry.1.swap.agg_pk == agg_pk)
            .map(|entry| entry.0)
    }

    /// Take a verified swap on: fund its receive contract in
    /// `destination`, log `BrokerSwapEvent`, and spawn the state machine
    /// that gathers the attestation and claims `send`, the contract the
    /// source mint holds at the request's outpoint. Idempotent via the
    /// caller's upstream markers, committed in the same dbtx.
    pub fn swap_broker_swap(
        &self,
        destination: MintId,
        dbtx: &WriteTx,
        operation: OperationId,
        req: SwapRequest,
        send: SendContract,
    ) -> anyhow::Result<()> {
        let ctx = self.ctx(destination)?;

        let amount = req.receive.amount;
        let fee = send.amount.saturating_sub(amount);
        let id = req.receive.id();

        let tx_builder = TxBuilder::from_output(Output {
            output: wire::Output::Swap(SwapOutput::Receive(req.receive)),
            amount,
            fee: ctx.config.swap.output_fee,
        });

        let txid = crate::ecash::finalize_and_submit_tx(
            &ctx,
            dbtx,
            ROUTING_ACCOUNT,
            operation,
            tx_builder,
            Vec::new(),
            false,
            |txid| BrokerSwapEvent { txid, amount, fee },
        )
        .context("Insufficient funds")?;

        crate::executor::add_state_machine_dbtx(
            &ctx,
            BrokerStateMachineTable,
            dbtx,
            BrokerStateMachine {
                operation,
                txid,
                id,
                source: req.mint,
                outpoint: req.outpoint,
                send,
            },
        );

        Ok(())
    }

    /// Await the outcome of a swap: the attestation once the send contract
    /// is claimed with it, `None` if the swap failed. Replays history, so a
    /// settled operation returns immediately, and a swap with neither an
    /// outcome nor a state machine left to produce one — its mint was
    /// removed — is a failure rather than a wait without end.
    pub async fn swap_broker_subscribe(&self, operation: OperationId) -> Option<Signature> {
        if !self.operation_is_active(operation)
            && !self
                .read_operation_events(operation)
                .iter()
                .any(|entry| entry.to_event::<BrokerSuccessEvent>().is_some())
        {
            return None;
        }

        let mut stream = self.subscribe_operation_events(operation);

        while let Some(entry) = stream.next().await {
            if let Some(ev) = entry.to_event::<BrokerSuccessEvent>() {
                return Some(ev.attestation);
            }

            if entry.to_event::<BrokerFailureEvent>().is_some() {
                return None;
            }
        }

        unreachable!("subscribe_operation_events only ends at client shutdown")
    }
}
