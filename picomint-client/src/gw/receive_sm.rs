use picomint_redb::{WriteTx, table};
use std::collections::BTreeMap;

use anyhow::{Context, anyhow};
use picomint_core::config::FederationId;
use picomint_core::core::OperationId;
use picomint_core::ln::LightningInput;
use picomint_core::ln::contracts::IncomingOffer;
use picomint_core::ln::methods::{DecryptionKeyShareRequest, DecryptionKeyShareResponse, LnMethod};
use picomint_core::module::Method;
use picomint_core::secp256k1::Keypair;
use picomint_core::wire;
use picomint_core::{OutPoint, PeerId};
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
    (FederationId, SmId) => ReceiveStateMachine,
    "gw-receive-sm",
);

/// Single-state state machine covering the federation side of the receive
/// flow. `trigger` waits for tx acceptance and gathers TPE decryption shares;
/// `transition` logs the terminal receive event and submits the refund tx
/// if the preimage decode failed. All external (LN / cross-fed) side effects
/// are handled out-of-band by the per-federation trailer task watching this
/// federation's event log.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Decodable, Encodable)]
pub struct ReceiveStateMachine {
    pub operation: OperationId,
    pub offer: IncomingOffer,
    pub outpoint: OutPoint,
    pub refund_keypair: Keypair,
}

impl StateMachine for ReceiveStateMachine {
    type Outcome = Result<BTreeMap<PeerId, DecryptionKeyShare>, String>;

    async fn trigger(&self, ctx: &ClientContext) -> Self::Outcome {
        ctx.await_tx_accepted(self.operation, self.outpoint.txid)
            .await
            .map_err(|e| e.to_string())?;

        let tpe_pks = ctx.config.ln.tpe_pks.clone();
        let offer = self.offer.clone();
        let shares = ctx
            .api
            .request_with_strategy_retry(
                FilterMapThreshold::new(
                    move |peer, resp: DecryptionKeyShareResponse| {
                        let share = resp.share;
                        if !offer.verify_decryption_share(
                            tpe_pks.get(&peer).context("Missing TPE PK for peer")?,
                            &share,
                        ) {
                            return Err(anyhow!("Invalid decryption share"));
                        }
                        Ok(share)
                    },
                    ctx.api.num_peers(),
                ),
                Method::Ln(LnMethod::DecryptionKeyShare(DecryptionKeyShareRequest {
                    outpoint: self.outpoint,
                })),
            )
            .await;

        Ok(shares)
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
                    super::GATEWAY_ACCOUNT,
                    self.operation,
                    ReceiveFailureEvent,
                );
                return None;
            }
            Ok(shares) => shares,
        };

        let decryption_shares: BTreeMap<u64, DecryptionKeyShare> = shares
            .into_iter()
            .map(|(peer, share)| (peer.to_usize() as u64, share))
            .collect();
        let agg_decryption_key = aggregate_dk_shares(&decryption_shares);

        if !self
            .offer
            .verify_agg_decryption_key(&ctx.config.ln.tpe_agg_pk, &agg_decryption_key)
        {
            warn!("Aggregate decryption key invalid — TPE config inconsistent");
            ctx.log_event(
                dbtx,
                super::GATEWAY_ACCOUNT,
                self.operation,
                ReceiveFailureEvent,
            );
            return None;
        }

        if let Some(preimage) = self.offer.decrypt_preimage(&agg_decryption_key) {
            ctx.log_event(
                dbtx,
                super::GATEWAY_ACCOUNT,
                self.operation,
                ReceiveSuccessEvent { preimage },
            );
            return None;
        }

        let tx_builder = TxBuilder::from_input(Input {
            input: wire::Input::Ln(LightningInput::Incoming(self.outpoint, agg_decryption_key)),
            keypair: self.refund_keypair,
            amount: self.offer.commitment.amount - self.offer.commitment.fee,
            fee: ctx.config.ln.input_fee,
        });

        crate::mint::finalize_and_submit_tx(
            ctx,
            dbtx,
            super::GATEWAY_ACCOUNT,
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
