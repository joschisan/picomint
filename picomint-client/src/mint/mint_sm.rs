use picomint_redb::WriteTx;
use std::collections::BTreeMap;

use crate::executor::{SmId, StateMachine};
use anyhow::ensure;
use picomint_core::core::OperationId;
use picomint_core::mint::{Denomination, verify_note};
use picomint_core::{PeerId, TransactionId};
use picomint_encoding::{Decodable, Encodable};
use tbs::{BlindedSignatureShare, PublicKeyShare, aggregate_signature_shares};

use super::client_db::NoteTable;
use super::events::{MintFailureEvent, MintSuccessEvent};
use super::{MintSmContext, NoteIssuanceRequest, SpendableNote};

crate::client_table!(
    MintStateMachineTable,
    SmId => MintStateMachine,
    "mint-mint-sm",
);

#[derive(Debug, Clone, Eq, PartialEq, Hash, Decodable, Encodable)]
pub struct MintStateMachine {
    pub operation: OperationId,
    /// Notes consumed on the input side that originated from our own
    /// wallet db (or, for recovery, were materialised from recovered
    /// nonces). Re-inserted into `NoteTable` on tx rejection.
    pub spendable_notes: Vec<SpendableNote>,
    /// Tx the SM is tied to. Recovery now submits a real reissuance
    /// tx, so this is always set.
    pub txid: TransactionId,
    /// Blinded outputs this tx issues. Finalized into `SpendableNote`s and
    /// inserted into `NoteTable` once the federation's blind-signature shares are
    /// aggregated.
    pub issuance_requests: Vec<NoteIssuanceRequest>,
}

picomint_redb::consensus_value!(MintStateMachine);

impl StateMachine for MintStateMachine {
    type Context = MintSmContext;
    type Outcome = Result<BTreeMap<PeerId, Vec<BlindedSignatureShare>>, String>;

    async fn trigger(&self, ctx: &Self::Context) -> Self::Outcome {
        ctx.client_ctx
            .await_tx_accepted(self.operation, self.txid)
            .await?;

        let shares = ctx
            .client_ctx
            .api()
            .signature_shares(
                self.txid,
                self.issuance_requests.clone(),
                ctx.tbs_pks.clone(),
            )
            .await;

        Ok(shares)
    }

    fn transition(
        &self,
        ctx: &Self::Context,
        dbtx: &WriteTx,
        outcome: Self::Outcome,
    ) -> Option<Self> {
        let Ok(signature_shares) = outcome else {
            for note in &self.spendable_notes {
                dbtx.insert(&NoteTable(ctx.federation), note, &());
            }

            return None;
        };

        for (i, request) in self.issuance_requests.iter().enumerate() {
            let agg_blind_signature = aggregate_signature_shares(
                &signature_shares
                    .iter()
                    .map(|(peer, shares)| (peer.to_usize() as u64, shares[i]))
                    .collect(),
            );

            let spendable_note = request.finalize(agg_blind_signature);

            let pk = *ctx
                .tbs_agg_pks
                .get(&request.denomination)
                .expect("No aggregated pk found for denomination");

            if !verify_note(spendable_note.note(), pk) {
                ctx.client_ctx
                    .log_event(dbtx, self.operation, MintFailureEvent);

                return None;
            }

            assert!(
                dbtx.insert(&NoteTable(ctx.federation), &spendable_note, &())
                    .is_none()
            );
        }

        let event = MintSuccessEvent {
            txid: self.txid,
            amount: self
                .issuance_requests
                .iter()
                .map(|r| r.denomination.amount())
                .sum(),
        };

        ctx.client_ctx.log_event(dbtx, self.operation, event);

        None
    }
}

pub fn verify_blind_shares(
    peer: PeerId,
    signature_shares: Vec<BlindedSignatureShare>,
    issuance_requests: &[NoteIssuanceRequest],
    tbs_pks: &BTreeMap<Denomination, BTreeMap<PeerId, PublicKeyShare>>,
) -> anyhow::Result<Vec<BlindedSignatureShare>> {
    ensure!(
        signature_shares.len() == issuance_requests.len(),
        "Invalid number of signatures shares"
    );

    for (request, share) in issuance_requests.iter().zip(signature_shares.iter()) {
        let amount_key = tbs_pks
            .get(&request.denomination)
            .expect("No pk shares found for denomination")
            .get(&peer)
            .expect("No pk share found for peer");

        ensure!(
            tbs::verify_signature_share(request.blinded_message(), *share, *amount_key),
            "Invalid blind signature"
        );
    }

    Ok(signature_shares)
}
