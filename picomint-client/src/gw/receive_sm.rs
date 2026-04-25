use std::collections::BTreeMap;

use anyhow::anyhow;
use picomint_core::core::OperationId;
use picomint_core::ln::LightningInput;
use picomint_core::ln::contracts::IncomingContract;
use picomint_core::ln::methods::{DecryptionKeyShareRequest, DecryptionKeyShareResponse, LnMethod};
use picomint_core::module::Method;
use picomint_core::secp256k1::Keypair;
use picomint_core::wire;
use picomint_core::{NumPeersExt, OutPoint, PeerId};
use picomint_encoding::{Decodable, Encodable};
use picomint_logging::LOG_CLIENT_MODULE_GW;
use picomint_redb::WriteTxRef;
use tpe::{DecryptionKeyShare, aggregate_dk_shares};
use tracing::warn;

use super::GwSmContext;
use super::events::{ReceiveFailureEvent, ReceiveRefundEvent, ReceiveSuccessEvent};
use crate::api::ServerError;
use crate::executor::StateMachine;
use crate::query::FilterMapThreshold;
use crate::transaction::{Input, TransactionBuilder};

/// Single-state state machine covering the federation side of the receive
/// flow. `trigger` waits for tx acceptance and gathers TPE decryption shares;
/// `transition` logs the terminal receive event and submits the refund tx
/// if the preimage decode failed. All external (LN / cross-fed) side effects
/// are handled out-of-band by the per-federation trailer task watching this
/// federation's event log.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Decodable, Encodable)]
pub struct ReceiveStateMachine {
    pub operation_id: OperationId,
    pub contract: IncomingContract,
    pub outpoint: OutPoint,
    pub refund_keypair: Keypair,
}

picomint_redb::consensus_value!(ReceiveStateMachine);

impl StateMachine for ReceiveStateMachine {
    const TABLE_NAME: &'static str = "gw-receive-sm";

    type Context = GwSmContext;
    type Outcome = Result<BTreeMap<PeerId, DecryptionKeyShare>, String>;

    async fn trigger(&self, ctx: &Self::Context) -> Self::Outcome {
        ctx.client_ctx
            .await_tx_accepted(self.operation_id, self.outpoint.txid)
            .await
            .map_err(|e| e.to_string())?;

        let tpe_pks = ctx.tpe_pks.clone();
        let contract = self.contract.clone();
        let shares = ctx
            .client_ctx
            .api()
            .request_with_strategy_retry(
                FilterMapThreshold::new(
                    move |peer_id, resp: DecryptionKeyShareResponse| {
                        let share = resp.share;
                        if !contract.verify_decryption_share(
                            tpe_pks
                                .get(&peer_id)
                                .ok_or(ServerError::InternalClientError(anyhow!(
                                    "Missing TPE PK for peer {peer_id}?!"
                                )))?,
                            &share,
                        ) {
                            return Err(ServerError::InvalidResponse(anyhow!(
                                "Invalid decryption share"
                            )));
                        }
                        Ok(share)
                    },
                    ctx.client_ctx.api().all_peers().to_num_peers(),
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
        ctx: &Self::Context,
        dbtx: &WriteTxRef<'_>,
        outcome: Self::Outcome,
    ) -> Option<Self> {
        let shares = match outcome {
            Err(_) => {
                ctx.client_ctx
                    .log_event(dbtx, self.operation_id, ReceiveFailureEvent);
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
            .contract
            .verify_agg_decryption_key(&ctx.tpe_agg_pk, &agg_decryption_key)
        {
            warn!(
                target: LOG_CLIENT_MODULE_GW,
                "Aggregate decryption key invalid — TPE config inconsistent"
            );
            ctx.client_ctx
                .log_event(dbtx, self.operation_id, ReceiveFailureEvent);
            return None;
        }

        if let Some(preimage) = self.contract.decrypt_preimage(&agg_decryption_key) {
            ctx.client_ctx
                .log_event(dbtx, self.operation_id, ReceiveSuccessEvent { preimage });
            return None;
        }

        let tx_builder = TransactionBuilder::from_input(Input {
            input: wire::Input::Ln(LightningInput::Incoming(self.outpoint, agg_decryption_key)),
            keypair: self.refund_keypair,
            amount: self.contract.commitment.amount,
            fee: ctx.input_fee,
        });

        let txid = ctx
            .mint
            .finalize_and_submit_transaction(dbtx, self.operation_id, tx_builder)
            .expect("Cannot claim input, additional funding needed");

        ctx.client_ctx
            .log_event(dbtx, self.operation_id, ReceiveRefundEvent { txid });
        None
    }
}
