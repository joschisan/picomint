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
use super::events::{IssuanceFailureEvent, IssuanceSuccessEvent};
use super::{NoteIssuanceRequest, SpendableNote};
use crate::context::ClientContext;

table!(
    EcashStateMachineTable,
    (MintId, SmId) => EcashStateMachine,
    "ecash-issuance-sm",
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

/// Outcome produced by [`EcashStateMachine::trigger`].
pub enum IssuanceOutcome {
    /// The tx was rejected; the notes it spent go back into `NoteTable`.
    Rejected,
    /// The tx was accepted and every issuance request finalized into a
    /// note that verifies against the mint's aggregate key, in request
    /// order.
    Issued(Vec<(Account, SpendableNote)>),
    /// The tx was accepted but a finalized note fails verification: the
    /// nodes' shares aggregated into something the mint won't honour.
    Invalid,
}

impl StateMachine for EcashStateMachine {
    type Outcome = IssuanceOutcome;

    async fn trigger(&self, ctx: &ClientContext) -> Self::Outcome {
        if ctx
            .await_tx_accepted(self.operation, self.txid)
            .await
            .is_err()
        {
            return IssuanceOutcome::Rejected;
        }

        // A tx without ecash outputs (the spent notes exactly covered its
        // deficit) still runs this machine to restore the notes on
        // rejection, but it has nothing to be signed — and the nodes'
        // `SignatureShares` long-poll never resolves for such a txid.
        // Asking would pin one stream per node for the life of the
        // process, and enough of those exhaust the per-connection stream
        // budget and stall every other request.
        if self.issuance_requests.is_empty() {
            return IssuanceOutcome::Issued(Vec::new());
        }

        let signatures = super::api::signature_shares(
            &ctx.api,
            self.txid,
            self.issuance_requests.clone(),
            ctx.config.ecash.tbs_pks.clone(),
        )
        .await;

        // Aggregation and verification are pairings per note, so they run
        // here rather than in `transition`: the transition holds the
        // database's single write lock, and every other state machine's
        // commit — a tx accept in particular — would queue behind them.
        // And on the blocking pool rather than a worker, so a burst of
        // them doesn't stall the runtime either.
        let requests = self.issuance_requests.clone();
        let agg_pks = ctx.config.ecash.tbs_agg_pks.clone();

        tokio::task::spawn_blocking(move || {
            let mut notes = Vec::new();

            for (i, request) in requests.iter().enumerate() {
                let agg_blind_signature = aggregate_signature_shares(
                    &signatures
                        .iter()
                        .map(|(node, shares)| (node.to_usize() as u64, shares[i]))
                        .collect(),
                );

                let spendable_note = request.finalize(agg_blind_signature);

                let pk = *agg_pks
                    .get(&request.denomination)
                    .expect("No aggregated pk found for denomination");

                if !verify_note(spendable_note.note(), pk) {
                    return IssuanceOutcome::Invalid;
                }

                notes.push((request.account(), spendable_note));
            }

            IssuanceOutcome::Issued(notes)
        })
        .await
        .expect("Note verification cannot panic")
    }

    fn transition(
        &self,
        ctx: &ClientContext,
        dbtx: &WriteTx,
        outcome: Self::Outcome,
    ) -> Option<Self> {
        match outcome {
            IssuanceOutcome::Rejected => {
                for note in &self.spendable_notes {
                    dbtx.insert_new(&NoteTable, &(ctx.mint, self.account, note.into()), &());
                }
            }
            IssuanceOutcome::Invalid => {
                ctx.log_event(dbtx, self.account, self.operation, IssuanceFailureEvent);
            }
            IssuanceOutcome::Issued(notes) if notes.is_empty() => {}
            IssuanceOutcome::Issued(notes) => {
                // The log entry is filed under this state machine's account, so it
                // reports what that account received — not what a fee output filed
                // elsewhere in the same transaction did.
                let event = IssuanceSuccessEvent {
                    txid: self.txid,
                    amount: notes
                        .iter()
                        .filter(|entry| entry.0 == self.account)
                        .map(|entry| entry.1.amount())
                        .sum(),
                };

                for (account, note) in notes {
                    dbtx.insert_new(&NoteTable, &(ctx.mint, account, (&note).into()), &());
                }

                ctx.log_event(dbtx, self.account, self.operation, event);
            }
        }

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
