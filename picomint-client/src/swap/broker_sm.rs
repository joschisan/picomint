use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, anyhow};
use picomint_core::config::MintId;
use picomint_core::core::OperationId;
use picomint_core::methods::Method;
use picomint_core::swap::methods::{AttestationShareRequest, AttestationShareResponse, SwapMethod};
use picomint_core::swap::{ReceiveContractId, SendContract, SwapInput};
use picomint_core::{NodeId, OutPoint, TransactionId, wire};
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::{WriteTx, table};
use picomint_rpc::query::FilterMapThreshold;
use tbs::{BlindedNonce, BlindedSignatureShare, Signature};
use tracing::warn;

use super::events::{BrokerFailureEvent, BrokerSuccessEvent};
use crate::context::ClientContext;
use crate::executor::{SmId, StateMachine};
use crate::gateway::ROUTING_ACCOUNT;
use crate::tx::{Input, TxBuilder};

table!(
    BrokerStateMachineTable,
    (MintId, SmId) => BrokerStateMachine,
    "swap-broker-sm",
);

/// Single-state state machine carrying a broker's swap from the funding
/// of the receive contract to the claim of the send contract. It lives in
/// the destination mint, where the funding and the shares are; the claim
/// is a transaction in the source mint, reached as a sibling context and
/// submitted in the same dbtx as this machine's transition.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Decodable, Encodable)]
pub struct BrokerStateMachine {
    pub operation: OperationId,
    /// The funding transaction, in the destination mint.
    pub txid: TransactionId,
    pub id: ReceiveContractId,
    pub source: MintId,
    pub outpoint: OutPoint,
    pub send: SendContract,
}

impl StateMachine for BrokerStateMachine {
    type Outcome = Result<BTreeMap<NodeId, BlindedSignatureShare>, String>;

    async fn trigger(&self, ctx: &ClientContext) -> Self::Outcome {
        let pks = Arc::new(ctx.config.swap.pks.clone());
        let message = BlindedNonce(self.id.message().0);

        // The nodes hold the share request until the contract is
        // processed, so it goes out alongside the wait for the funding
        // transaction instead of after it. A threshold of verified shares
        // proves the contract was funded; a rejection drops the pending
        // request.
        let accepted = ctx.await_tx_accepted(self.operation, self.txid);

        let shares = ctx.api.request_with_strategy_retry(
            FilterMapThreshold::new(
                move |node, resp: AttestationShareResponse| {
                    let pks = pks.clone();

                    // A pairing per share; keep it off the runtime worker.
                    async move {
                        tokio::task::spawn_blocking(move || {
                            let pk = pks.get(&node).context("Missing swap pk for node")?;

                            if !tbs::verify_signature_share(message, resp.share, *pk) {
                                return Err(anyhow!("Invalid attestation share"));
                            }

                            Ok(resp.share)
                        })
                        .await
                        .expect("Share verification cannot panic")
                    }
                },
                ctx.api.num_nodes(),
            ),
            Method::Swap(SwapMethod::AttestationShare(AttestationShareRequest {
                outpoint: OutPoint {
                    txid: self.txid,
                    out_idx: 0,
                },
            })),
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
                ctx.log_event(dbtx, ROUTING_ACCOUNT, self.operation, BrokerFailureEvent);
                return None;
            }
            Ok(shares) => shares,
        };

        let shares: BTreeMap<u64, BlindedSignatureShare> = shares
            .into_iter()
            .map(|(node, share)| (node.to_usize() as u64, share))
            .collect();

        let attestation = Signature(tbs::aggregate_signature_shares(&shares).0);

        if !tbs::verify(self.id.message(), attestation, ctx.config.swap.agg_pk) {
            warn!("Aggregate attestation invalid — swap config inconsistent");
            ctx.log_event(dbtx, ROUTING_ACCOUNT, self.operation, BrokerFailureEvent);
            return None;
        }

        let Some(source) = ctx.sibling(self.source) else {
            warn!("The source mint was removed before the claim");
            ctx.log_event(dbtx, ROUTING_ACCOUNT, self.operation, BrokerFailureEvent);
            return None;
        };

        let tx_builder = TxBuilder::from_input(Input {
            input: wire::Input::Swap(SwapInput::ClaimSend(self.outpoint, attestation)),
            keypair: source.secret.swap_secret().claim_keypair(),
            amount: self.send.amount,
            fee: source.config.swap.input_fee,
        });

        crate::ecash::finalize_and_submit_tx(
            &source,
            dbtx,
            ROUTING_ACCOUNT,
            self.operation,
            tx_builder,
            Vec::new(),
            false,
            |txid| BrokerSuccessEvent { attestation, txid },
        )
        .expect("Cannot claim send contract — additional funding needed");

        None
    }
}
