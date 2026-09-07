use picomint_redb::{WriteTx, table};
use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, anyhow};
use picomint_core::config::MintId;
use picomint_core::core::OperationId;
use picomint_core::lightning::LightningInput;
use picomint_core::lightning::contracts::IncomingOffer;
use picomint_core::lightning::methods::{
    DecryptionKeyShareRequest, DecryptionKeyShareResponse, LightningMethod,
};
use picomint_core::methods::Method;
use picomint_core::secp256k1::Keypair;
use picomint_core::wire;
use picomint_core::{NodeId, OutPoint};
use picomint_encoding::{Decodable, Encodable};
use tpe::{DecryptionKeyShare, aggregate_dk_shares};
use tracing::warn;

use super::events::{ReceiveFailureEvent, ReceiveRefundEvent, ReceiveSuccessEvent};
use crate::context::ClientContext;
use crate::executor::{SmId, StateMachine};
use crate::tx::{Input, TxBuilder};
use picomint_rpc::query::FilterMapThreshold;

table!(
    ReceiveStateMachineTable,
    (MintId, SmId) => ReceiveStateMachine,
    "gateway-receive-sm",
);

/// Single-state state machine covering the mint side of the receive
/// flow. `trigger` waits for tx acceptance and gathers TPE decryption shares;
/// `transition` logs the terminal receive event and submits the refund tx
/// if the preimage decode failed. All external (LN / cross-mint) side effects
/// are handled out-of-band by the per-mint trailer task watching this
/// mint's event log.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Decodable, Encodable)]
pub struct ReceiveStateMachine {
    pub operation: OperationId,
    pub offer: IncomingOffer,
    pub outpoint: OutPoint,
    pub refund_keypair: Keypair,
}

impl StateMachine for ReceiveStateMachine {
    type Outcome = Result<BTreeMap<NodeId, DecryptionKeyShare>, String>;

    async fn trigger(&self, ctx: &ClientContext) -> Self::Outcome {
        let tpe_pks = Arc::new(ctx.config.lightning.tpe_pks.clone());
        let offer = Arc::new(self.offer.clone());

        // The nodes hold the share request until the contract is
        // processed, so it goes out alongside the wait for the funding
        // transaction instead of after it: one round trip less per
        // receive. A threshold of verified shares proves the contract
        // was funded; a rejection drops the pending request.
        let accepted = ctx.await_tx_accepted(self.operation, self.outpoint.txid);

        let shares = ctx.api.request_with_strategy_retry(
            FilterMapThreshold::new(
                move |node, resp: DecryptionKeyShareResponse| {
                    let tpe_pks = tpe_pks.clone();
                    let offer = offer.clone();

                    // A pairing per share; keep it off the runtime worker.
                    async move {
                        tokio::task::spawn_blocking(move || {
                            let share = resp.share;
                            if !offer.verify_decryption_share(
                                tpe_pks.get(&node).context("Missing TPE PK for node")?,
                                &share,
                            ) {
                                return Err(anyhow!("Invalid decryption share"));
                            }
                            Ok(share)
                        })
                        .await
                        .expect("Share verification cannot panic")
                    }
                },
                ctx.api.num_nodes(),
            ),
            Method::Lightning(LightningMethod::DecryptionKeyShare(
                DecryptionKeyShareRequest {
                    outpoint: self.outpoint,
                },
            )),
        );

        tokio::pin!(accepted);
        tokio::pin!(shares);

        tokio::select! {
            result = &mut accepted => {
                result.map_err(|e| e.to_string())?;

                Ok(shares.await)
            }
            shares = &mut shares => Ok(shares),
        }
    }

    fn transition(
        &self,
        ctx: &ClientContext,
        dbtx: &WriteTx,
        outcome: Self::Outcome,
    ) -> Option<Self> {
        let shares = match outcome {
            Err(_) => {
                ctx.log_event(
                    dbtx,
                    super::ROUTING_ACCOUNT,
                    self.operation,
                    ReceiveFailureEvent,
                );
                return None;
            }
            Ok(shares) => shares,
        };

        let decryption_shares: BTreeMap<u64, DecryptionKeyShare> = shares
            .into_iter()
            .map(|(node, share)| (node.to_usize() as u64, share))
            .collect();
        let Some(agg_decryption_key) = aggregate_dk_shares(&decryption_shares).filter(|key| {
            self.offer
                .verify_agg_decryption_key(&ctx.config.lightning.tpe_agg_pk, key)
        }) else {
            warn!("Aggregate decryption key invalid — TPE config inconsistent");
            ctx.log_event(
                dbtx,
                super::ROUTING_ACCOUNT,
                self.operation,
                ReceiveFailureEvent,
            );
            return None;
        };

        if let Some(preimage) = self.offer.decrypt_preimage(&agg_decryption_key) {
            ctx.log_event(
                dbtx,
                super::ROUTING_ACCOUNT,
                self.operation,
                ReceiveSuccessEvent { preimage },
            );
            return None;
        }

        let tx_builder = TxBuilder::from_input(Input {
            input: wire::Input::Lightning(LightningInput::Incoming(
                self.outpoint,
                agg_decryption_key,
            )),
            keypair: self.refund_keypair,
            amount: self.offer.commitment.amount - self.offer.commitment.fee,
            fee: ctx.config.lightning.input_fee,
        });

        crate::ecash::finalize_and_submit_tx(
            ctx,
            dbtx,
            super::ROUTING_ACCOUNT,
            self.operation,
            tx_builder,
            Vec::new(),
            false,
            |txid| ReceiveRefundEvent { txid },
        )
        .expect("Cannot claim input, additional funding needed");

        None
    }
}
