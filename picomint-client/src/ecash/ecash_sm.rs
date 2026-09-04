use picomint_redb::{WriteTx, table};
use std::collections::BTreeMap;

use crate::executor::{SmId, StateMachine};
use anyhow::ensure;
use picomint_core::config::MintId;
use picomint_core::core::{Account, OperationId};
use picomint_core::ecash::{Denomination, verify_note};
use picomint_core::{NodeId, TransactionId};
use picomint_encoding::{Decodable, Encodable};
use tbs::{BlindedSignatureShare, PublicKeyShare, aggregate_signature_shares};

use super::client_db::NoteTable;
use super::events::{EcashFailureEvent, EcashSuccessEvent};
use super::{NoteIssuanceRequest, SpendableNote};
use crate::context::ClientContext;

table!(
    EcashStateMachineTable,
    (MintId, SmId) => EcashStateMachine,
    "ecash-sm",
);

#[derive(Debug, Clone, Eq, PartialEq, Hash, Decodable, Encodable)]
pub struct EcashStateMachine {
    /// Account whose balance this issuance settles into. Carried in the state
    /// rather than in the table name, so one executor drives every account's
    /// state machines.
    pub account: Account,
    pub operation: OperationId,
    /// Notes consumed on the input side that came out of our own
    /// `NoteTable`, and are re-inserted there on tx rejection. A restore's
    /// notes do not travel here — they are credited to `NoteTable` directly
    /// by `commit_scan`.
    pub spendable_notes: Vec<SpendableNote>,
    /// Tx the SM is tied to.
    pub txid: TransactionId,
    /// Blinded outputs this tx issues. Finalized into `SpendableNote`s and
    /// inserted into `NoteTable` once the mint's blind-signature shares are
    /// aggregated.
    ///
    /// Each carries the account it settles into, which is not always
    /// [`Self::account`]: a client configured with a fee pays it as an
    /// output of the transaction it is charging.
    pub issuance_requests: Vec<NoteIssuanceRequest>,
}

impl StateMachine for EcashStateMachine {
    type Outcome = Result<BTreeMap<NodeId, Vec<BlindedSignatureShare>>, String>;

    async fn trigger(&self, ctx: &ClientContext) -> Self::Outcome {
        ctx.await_tx_accepted(self.operation, self.txid).await?;

        let shares = super::api::signature_shares(
            &ctx.api,
            self.txid,
            self.issuance_requests.clone(),
            ctx.config.ecash.tbs_pks.clone(),
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
        let Ok(signatures) = outcome else {
            for note in &self.spendable_notes {
                dbtx.insert(&NoteTable, &(ctx.mint, self.account, note.clone()), &());
            }

            return None;
        };

        for (i, request) in self.issuance_requests.iter().enumerate() {
            let agg_blind_signature = aggregate_signature_shares(
                &signatures
                    .iter()
                    .map(|(node, shares)| (node.to_usize() as u64, shares[i]))
                    .collect(),
            );

            let spendable_note = request.finalize(agg_blind_signature);

            let pk = *ctx
                .config
                .ecash
                .tbs_agg_pks
                .get(&request.denomination)
                .expect("No aggregated pk found for denomination");

            if !verify_note(spendable_note.note(), pk) {
                ctx.log_event(dbtx, self.account, self.operation, EcashFailureEvent);

                return None;
            }

            assert!(
                dbtx.insert(
                    &NoteTable,
                    &(ctx.mint, request.account(), spendable_note),
                    &()
                )
                .is_none()
            );
        }

        // The log entry is filed under this state machine's account, so it
        // reports what that account received — not what a fee output filed
        // elsewhere in the same transaction did.
        let event = EcashSuccessEvent {
            txid: self.txid,
            amount: self
                .issuance_requests
                .iter()
                .filter(|r| r.account() == self.account)
                .map(|r| r.denomination.amount())
                .sum(),
        };

        ctx.log_event(dbtx, self.account, self.operation, event);

        None
    }
}

pub fn verify_blind_shares(
    node: NodeId,
    signatures: Vec<BlindedSignatureShare>,
    issuance_requests: &[NoteIssuanceRequest],
    tbs_pks: &BTreeMap<Denomination, BTreeMap<NodeId, PublicKeyShare>>,
) -> anyhow::Result<Vec<BlindedSignatureShare>> {
    ensure!(
        signatures.len() == issuance_requests.len(),
        "Invalid number of signatures shares"
    );

    for (request, share) in issuance_requests.iter().zip(signatures.iter()) {
        let amount_key = tbs_pks
            .get(&request.denomination)
            .expect("No pk shares found for denomination")
            .get(&node)
            .expect("No pk share found for node");

        ensure!(
            tbs::verify_signature_share(request.blinded_nonce(), *share, *amount_key),
            "Invalid blind signature"
        );
    }

    Ok(signatures)
}
