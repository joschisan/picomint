pub use picomint_core::mint as common;

mod api;
mod client_db;
mod ecash;
mod events;
mod issuance;
mod mint_sm;
mod secret;
mod send_sm;

use picomint_redb::WriteTx;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::api::FederationApi;
use crate::executor::ModuleExecutor;
use crate::module::ClientContext;
use crate::task::TaskGroup;
use crate::tx::{Input, Output, TxBuilder};
use crate::tx::{TxSubmissionSmContext, TxSubmissionStateMachine, TxSubmissionStateMachineTable};
use anyhow::ensure;
use client_db::{CounterTable, NoteTable, ReceiveOperationTable};
pub use events::*;
use futures::StreamExt;
use picomint_core::config::FederationId;
use picomint_core::core::{Account, OperationId};
use picomint_core::mint::config::{MintConfigConsensus, client_denominations};
use picomint_core::mint::{Denomination, MintInput, Note};
use picomint_core::secp256k1::{Keypair, XOnlyPublicKey};
use picomint_core::tx::Transaction;
use picomint_core::{Amount, PeerId, TransactionId, wire};
use picomint_encoding::{Decodable, Encodable};
use tbs::{AggregatePublicKey, aggregate_signature_shares};
use thiserror::Error;

pub use self::ecash::ECash;
use self::issuance::{NoteIssuance, NoteIssuanceRequest};
use self::mint_sm::{MintStateMachine, MintStateMachineTable};
pub use self::secret::MintSecret;
use self::send_sm::{SendStateMachine, SendStateMachineTable};

const TARGET_PER_DENOMINATION: usize = 3;

/// Counters probed per round trip — and, since a scan stops on its first
/// fully empty batch, the gap limit itself.
///
/// One counter space serves every denomination, so a transaction burns one
/// counter per output rather than one or two out of a space of its own. The
/// gap a scan refuses to cross therefore has to absorb whole transactions'
/// worth of counters at a time, which is why this sits well above both the
/// number of denominations and the outputs any one transaction can carry.
const RESTORE_BATCH: u64 = 500;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Encodable, Decodable)]
pub struct SpendableNote {
    pub denomination: Denomination,
    pub keypair: Keypair,
    pub signature: tbs::Signature,
}

picomint_redb::consensus_key!(SpendableNote);

impl SpendableNote {
    pub fn amount(&self) -> Amount {
        self.denomination.amount()
    }
}

impl SpendableNote {
    fn nonce(&self) -> XOnlyPublicKey {
        self.keypair.x_only_public_key().0
    }

    fn note(&self) -> Note {
        Note {
            denomination: self.denomination,
            nonce: self.nonce(),
            signature: self.signature,
        }
    }
}

/// Everything a seed-only scan of one account turned up: the notes it still
/// owns, and how far its counter space was walked.
///
/// Produced by [`crate::restore`], which touches no database at all. The
/// counter goes to [`commit_restore`] in a dbtx the caller owns, alongside
/// whatever marks the federation as added; the notes go to
/// [`MintClientModule::receive`] as an ordinary bundle once the client is up.
///
/// One of these covers the single account the scan was run for. A caller
/// restoring a whole client runs the scan once per [`Account`] and applies
/// each result under the account it asked for.
#[derive(Debug, Clone)]
pub struct Restore {
    federation: FederationId,
    notes: Vec<SpendableNote>,
    counter: u64,
}

impl Restore {
    /// Gross value restored, before the reissuance's fees.
    pub fn amount(&self) -> Amount {
        self.notes.iter().map(SpendableNote::amount).sum()
    }

    pub fn federation(&self) -> FederationId {
        self.federation
    }

    /// The restored notes as an out-of-band bundle, to hand straight to
    /// [`MintClientModule::receive`].
    ///
    /// Restore and an ordinary out-of-band receive are the same operation:
    /// notes that someone else may know traded for notes only this wallet
    /// does. Here the someone else is the federation, which was asked about
    /// every one of these nonces by name during the scan. Reissuing is what
    /// makes the balance the wallet's own, so it rides the existing path
    /// rather than a restore-shaped copy of it.
    ///
    /// Empty when the scan found nothing, which
    /// [`MintClientModule::receive`] rejects with
    /// [`ReceiveECashError::Empty`]; check [`Restore::amount`] first if a
    /// never-used seed is a case the caller expects.
    pub fn ecash(&self) -> ECash {
        ECash::new(self.federation, self.notes.clone())
    }
}

/// Persist the counter mark a [`scan`] reached for its account, in a dbtx the
/// caller owns.
///
/// This is the whole of what restore writes locally, and it must land before
/// the account issues anything: a restored account resuming from zero would
/// re-derive nonces the federation has already signed, and every note behind
/// them would be stranded. The notes themselves are not written here — they
/// arrive through [`MintClientModule::receive`] like any other bundle.
///
/// `account` is the one the scan was run for — the caller named it when
/// calling [`crate::restore`], so it is not carried on the result.
///
/// Belongs in the same dbtx as whatever marks the federation as added, so a
/// crash leaves either both or neither. A caller restoring every account can
/// commit all of their marks in that one dbtx.
pub fn commit_restore(dbtx: &WriteTx, account: Account, restore: &Restore) {
    dbtx.insert(
        &CounterTable(restore.federation),
        &account,
        &restore.counter,
    );
}

/// Rebuild a wallet's notes from its seed, without touching a database.
///
/// Runs in two phases. A single counter space serves every denomination, so
/// the walk is one sequential chain of batches, stopping as soon as one full
/// batch turns up nothing at all — neither a nonce the federation has seen
/// spent nor a blinded message it ever signed. Only once the live set has
/// settled are the signature shares fetched, in a single request.
///
/// Splitting membership from retrieval is what keeps a note from going
/// missing: both probes answer under threshold consensus, so peers must agree
/// before a counter is written off, and the fetch then asks only for messages
/// already known to resolve — a share the federation fails to produce is an
/// error rather than a candidate quietly dropped for want of a full column to
/// interpolate over.
pub(crate) async fn scan(
    api: &FederationApi,
    secret: &MintSecret,
    cfg: &MintConfigConsensus,
    federation: FederationId,
    account: Account,
) -> anyhow::Result<Restore> {
    let (counter, requests) = scan_counters(api, secret, account).await;

    let mut notes = Vec::with_capacity(requests.len());

    if !requests.is_empty() {
        let shares = api
            .signature_shares_restore(requests.clone(), cfg.tbs_pks.clone())
            .await;

        for (i, request) in requests.iter().enumerate() {
            let shares = shares
                .iter()
                .map(|(peer, peer_shares)| (peer.to_usize() as u64, peer_shares[i]))
                .collect();

            let note = request.finalize(aggregate_signature_shares(&shares));

            let pk = cfg
                .tbs_agg_pks
                .get(&note.denomination)
                .expect("No aggregated pk found for denomination");

            ensure!(
                picomint_core::mint::verify_note(note.note(), *pk),
                "Restored note failed verification against the aggregate public key"
            );

            notes.push(note);
        }
    }

    Ok(Restore {
        federation,
        notes,
        counter,
    })
}

/// Walk the counter space. Returns the counter the scan reached and the
/// candidates the federation both signed and has not seen spent — the notes
/// still live, whose shares the caller fetches in bulk.
///
/// A candidate's denomination comes back from [`FederationApi::issuance_state`]
/// rather than from the seed, since nothing under a single counter depends on
/// it. A peer that reports the wrong one cannot make the wallet credit itself
/// anything: the share is aggregated and checked against that denomination's
/// aggregate public key, which the real signature will not satisfy.
///
/// The counter returned is the *start* of the empty batch that ended the scan,
/// never its end. Both are safe to issue from — the whole batch probed clean —
/// but only the boundary keeps the wallet recoverable a second time: a later
/// scan gives up on its first empty batch, so leaving a full batch of unused
/// counters below the next issuance would strand it behind exactly the gap the
/// scan refuses to cross.
async fn scan_counters(
    api: &FederationApi,
    secret: &MintSecret,
    account: Account,
) -> (u64, Vec<NoteIssuanceRequest>) {
    let mut found = Vec::new();
    let mut counter = 0;

    loop {
        let candidates: Vec<NoteIssuance> = (counter..counter + RESTORE_BATCH)
            .map(|c| NoteIssuance::new(account, c, secret))
            .collect();

        let spent = api
            .spend_state(candidates.iter().map(NoteIssuance::nonce).collect())
            .await;

        // Deriving a blinded message costs some twenty times what the nonce
        // did, so it is paid only for counters that survived the spend check.
        let unspent: Vec<NoteIssuance> = candidates
            .into_iter()
            .zip(&spent)
            .filter(|(_, spent)| !**spent)
            .map(|(candidate, _)| candidate)
            .collect();

        let messages = tokio::task::spawn_blocking({
            let unspent = unspent.clone();

            move || unspent.iter().map(NoteIssuance::blinded_message).collect()
        })
        .await
        .expect("Blinded message derivation cannot panic");

        let issued = api.issuance_state(messages).await;

        if issued.iter().all(Option::is_none) && !spent.contains(&true) {
            return (counter, found);
        }

        found.extend(
            unspent
                .into_iter()
                .zip(&issued)
                .filter_map(|(candidate, issued)| issued.map(|d| candidate.request(d))),
        );

        counter += RESTORE_BATCH;
    }
}

impl MintClientModule {
    /// Hand out `account`'s next counter and persist the bump in the caller's
    /// dbtx, so a counter is only spent once the transaction carrying its
    /// blinded message is committed to.
    fn next_counter(&self, dbtx: &WriteTx, account: Account) -> u64 {
        let counter = dbtx
            .get(&CounterTable(self.federation), &account)
            .unwrap_or(0);

        dbtx.insert(&CounterTable(self.federation), &account, &(counter + 1));

        counter
    }

    /// Blinded outputs paying both cuts on the value a transaction moves, and
    /// the issuance requests that redeem them into
    /// [`Account::OperatorFee`] and [`Account::IntegratorFee`].
    ///
    /// Called on a builder holding its caller's outputs and no funding, where
    /// the imbalance is still the operation's own amount — a payment's
    /// outputs on the way out, a claim's inputs on the way in, and exactly one
    /// of the two. A transaction that moves nothing pays nothing.
    ///
    /// Both cuts are charged on that same amount rather than one on what the
    /// other left, so neither party's take depends on whether the other is
    /// charging.
    ///
    /// Spending a fee account is how its owner collects, and a collection
    /// pays neither cut — not its own, which would leave a remainder that can
    /// never be swept, and not the other party's, which would be a cut of
    /// money that was never theirs.
    fn add_fee_outputs(
        &self,
        dbtx: &WriteTx,
        account: Account,
        builder: &mut TxBuilder,
    ) -> Vec<NoteIssuanceRequest> {
        // On [`Account::User`] rather than on a list of the accounts to skip,
        // so an account added later is exempt by being what it is.
        if !matches!(account, Account::User(_)) {
            return Vec::new();
        }

        let basis = builder.deficit() + builder.excess_input();

        let cuts = [
            (Account::OperatorFee, self.operator_fee_ppm(dbtx)),
            (Account::IntegratorFee, self.integrator_fee_ppm),
        ];

        let mut requests = Vec::new();

        for (destination, ppm) in cuts {
            if ppm == 0 {
                continue;
            }

            requests.extend(self.fee_requests(dbtx, destination, basis, ppm));
        }

        for request in &requests {
            builder.add_output(Output {
                output: wire::Output::Mint(request.output()),
                amount: request.denomination.amount(),
                fee: self.cfg.output_fee,
            });
        }

        requests
    }

    /// The federation's announced cut, as this client last read it back.
    ///
    /// A local read rather than a query: the announcement is refreshed on its
    /// own schedule, so building a transaction never waits on the federation
    /// to answer what it charges.
    fn operator_fee_ppm(&self, dbtx: &WriteTx) -> u64 {
        dbtx.get(&crate::fee::OperatorFeeTable(self.federation), &())
            .map_or(0, |fee| fee.ppm)
    }

    /// One cut's worth of issuance requests, in denominations totalling
    /// `ppm` parts per million of `basis`.
    ///
    /// The cut absorbs the federation's fee on the outputs carrying it, so
    /// what is charged is what is charged; a cut too small to buy the
    /// smallest denomination buys nothing at all and is left behind as
    /// federation revenue, the same way change dust is.
    fn fee_requests(
        &self,
        dbtx: &WriteTx,
        destination: Account,
        basis: Amount,
        ppm: u64,
    ) -> Vec<NoteIssuanceRequest> {
        let cut = Amount::from_msat(basis.msat.saturating_mul(ppm).saturating_div(1_000_000));

        let mut denominations = Self::select_output_denominations(self.cfg.output_fee, cut);

        // Sorted for the same reason the change outputs are: the shape of a
        // transaction's outputs should say as little as possible about which
        // of them are whose.
        denominations.sort();

        denominations
            .into_iter()
            .map(|d| {
                let counter = self.next_counter(dbtx, destination);

                NoteIssuanceRequest::new(destination, d, counter, &self.secret)
            })
            .collect()
    }
}

/// What of `requests` settles into `account` — the value one cut actually
/// reached its account with, which is not the amount charged when a
/// denomination was too small to buy.
fn cut_into(requests: &[NoteIssuanceRequest], account: Account) -> Amount {
    requests
        .iter()
        .filter(|r| r.account() == account)
        .map(|r| r.denomination.amount())
        .sum()
}

impl MintClientModule {
    pub fn new(
        federation: FederationId,
        cfg: MintConfigConsensus,
        context: ClientContext,
        secret: MintSecret,
        integrator_fee_ppm: u64,
        tg: &TaskGroup,
    ) -> MintClientModule {
        let sm_context = MintSmContext {
            client_ctx: context.clone(),
            federation,
            tbs_agg_pks: cfg.tbs_agg_pks.clone(),
            tbs_pks: cfg.tbs_pks.clone(),
        };

        let mint_executor = ModuleExecutor::new(
            context.db().clone(),
            MintStateMachineTable(federation),
            sm_context.clone(),
            tg.clone(),
        );

        let send_executor = ModuleExecutor::new(
            context.db().clone(),
            SendStateMachineTable(federation),
            sm_context,
            tg.clone(),
        );

        let tx_submission_executor = ModuleExecutor::new(
            context.db().clone(),
            TxSubmissionStateMachineTable(federation),
            TxSubmissionSmContext {
                api: context.api(),
                federation,
                logger: context.logger().clone(),
            },
            tg.clone(),
        );

        MintClientModule {
            federation,
            cfg,
            secret,
            integrator_fee_ppm,
            client_ctx: context,
            tx_submission_executor,
            mint_executor,
            send_executor,
        }
    }
}

#[derive(Clone)]
pub struct MintClientModule {
    federation: FederationId,
    cfg: MintConfigConsensus,
    secret: MintSecret,
    /// Parts per million of the value each transaction moves that the client
    /// pays into [`Account::IntegratorFee`]. Zero for a client that charges nothing,
    /// which is every client the workspace itself builds.
    integrator_fee_ppm: u64,
    client_ctx: ClientContext,
    tx_submission_executor: ModuleExecutor<TxSubmissionStateMachine, TxSubmissionStateMachineTable>,
    mint_executor: ModuleExecutor<MintStateMachine, MintStateMachineTable>,
    send_executor: ModuleExecutor<SendStateMachine, SendStateMachineTable>,
}

/// Context handed to per-SM executors. Keeps the `ClientContext` handle
/// plus the immutable config data SMs need.
#[derive(Clone)]
pub struct MintSmContext {
    pub client_ctx: ClientContext,
    pub federation: FederationId,
    pub tbs_agg_pks: BTreeMap<Denomination, AggregatePublicKey>,
    pub tbs_pks: BTreeMap<Denomination, BTreeMap<PeerId, tbs::PublicKeyShare>>,
}

impl MintClientModule {
    pub fn input_fee(&self) -> Amount {
        self.cfg.input_fee
    }

    pub fn output_fee(&self) -> Amount {
        self.cfg.output_fee
    }

    /// Balance the builder against mint's wallet (pulling funding notes when
    /// underfunded, generating change outputs when overfunded), sign and
    /// submit the resulting transaction, and spawn the
    /// `MintStateMachine` that tracks the balance-side notes/requests
    /// (if any).
    ///
    /// `event` builds the module's initiating event (e.g. `SendEvent`)
    /// from the txid; this method logs it before the bookkeeping
    /// `TxCreateEvent` so the operation's event log opens with the
    /// module event.
    pub fn finalize_and_submit_tx<E: picomint_eventlog::Event + Send>(
        &self,
        dbtx: &WriteTx,
        account: Account,
        operation: OperationId,
        mut builder: TxBuilder,
        event: impl FnOnce(TransactionId) -> E,
    ) -> Option<TransactionId> {
        // Ahead of the deficit the funding has to cover, so the cut is funded
        // like any other output rather than out of the change.
        let mut issuance_requests = self.add_fee_outputs(dbtx, account, &mut builder);

        let operator_fee = cut_into(&issuance_requests, Account::OperatorFee);
        let integrator_fee = cut_into(&issuance_requests, Account::IntegratorFee);

        let deficit = builder.deficit();

        let (spendable_notes, change_requests) = self.balance(dbtx, account, &mut builder)?;

        issuance_requests.extend(change_requests);

        let funding: Amount = spendable_notes.iter().map(|n| n.amount()).sum();

        let remint = funding.saturating_sub(deficit);

        let txid = self.submit(
            dbtx,
            account,
            operation,
            builder,
            remint,
            operator_fee,
            integrator_fee,
            event,
        );

        if !spendable_notes.is_empty() || !issuance_requests.is_empty() {
            let sm = MintStateMachine {
                account,
                operation,
                spendable_notes,
                txid,
                issuance_requests,
            };
            self.mint_executor.add_state_machine_dbtx(dbtx, sm);
        }

        Some(txid)
    }

    /// Mint-side transaction balancing. Pulls funding notes from `account`
    /// when the builder is underfunded, then absorbs any excess as change
    /// outputs issued back to the same account. Sub-denomination dust below
    /// `smallest_denom + output_fee` is left as implicit federation revenue.
    /// Returns `None` iff the account holds insufficient funds to cover the
    /// builder's deficit, which is the only way balancing fails.
    fn balance(
        &self,
        dbtx: &WriteTx,
        account: Account,
        builder: &mut TxBuilder,
    ) -> Option<(Vec<SpendableNote>, Vec<NoteIssuanceRequest>)> {
        let mut spendable_notes = self.select_funding_input(dbtx, account, builder.deficit())?;

        // Sort by denomination to minimize information leaked about
        // which notes the wallet held.
        spendable_notes.sort_by_key(|note| note.denomination);

        for note in &spendable_notes {
            Self::remove_spendable_note(dbtx, self.federation, account, note);
            builder.add_input(Input {
                input: wire::Input::Mint(MintInput { note: note.note() }),
                keypair: note.keypair,
                amount: note.amount(),
                fee: self.cfg.input_fee,
            });
        }

        assert_eq!(builder.deficit(), Amount::ZERO);

        let mut denoms =
            Self::select_output_denominations(self.cfg.output_fee, builder.excess_input());

        // Sort to minimize information leaked about the change shape.
        denoms.sort();

        let mut issuance_requests = Vec::new();

        for d in denoms {
            let counter = self.next_counter(dbtx, account);

            issuance_requests.push(NoteIssuanceRequest::new(account, d, counter, &self.secret));
        }

        for request in &issuance_requests {
            builder.add_output(Output {
                output: wire::Output::Mint(request.output()),
                amount: request.denomination.amount(),
                fee: self.cfg.output_fee,
            });
        }

        assert_eq!(builder.deficit(), Amount::ZERO);

        Some((spendable_notes, issuance_requests))
    }

    /// Sign the builder, spawn the `TxSubmissionStateMachine`, log the
    /// caller's `event` followed by `TxCreateEvent`.
    #[allow(clippy::too_many_arguments)]
    fn submit<E: picomint_eventlog::Event + Send>(
        &self,
        dbtx: &WriteTx,
        account: Account,
        operation: OperationId,
        builder: TxBuilder,
        remint: Amount,
        operator_fee: Amount,
        integrator_fee: Amount,
        event: impl FnOnce(TransactionId) -> E,
    ) -> TransactionId {
        let tx_fee = builder.total_fee();
        let tx = builder.build();

        let txid = tx.compute_txid();

        let sm = TxSubmissionStateMachine {
            account,
            operation,
            tx,
        };

        self.tx_submission_executor.add_state_machine_dbtx(dbtx, sm);

        self.client_ctx
            .log_event(dbtx, account, operation, event(txid));

        self.client_ctx.log_event(
            dbtx,
            account,
            operation,
            crate::TxCreateEvent {
                txid,
                remint,
                tx_fee,
                operator_fee,
                integrator_fee,
            },
        );

        txid
    }

    pub fn get_balance(&self, dbtx: &impl picomint_redb::DbRead, account: Account) -> Amount {
        Self::get_count_by_denomination_dbtx(dbtx, self.federation, account)
            .into_iter()
            .map(|(denomination, count)| denomination.amount().mul_u64(count))
            .sum()
    }

    pub fn balance_notify(&self) -> Arc<tokio::sync::Notify> {
        self.client_ctx
            .db()
            .notify_for_table(&NoteTable(self.federation))
    }

    fn select_funding_input(
        &self,
        dbtx: &WriteTx,
        account: Account,
        excess_output: Amount,
    ) -> Option<Vec<SpendableNote>> {
        let mut selected = Vec::new();
        let mut target_notes = Vec::new();

        let all_notes = account_notes(dbtx, self.federation, account);

        for amount in client_denominations().rev() {
            let notes_amount: Vec<SpendableNote> = all_notes
                .iter()
                .filter(|note| note.denomination == amount)
                .cloned()
                .collect();

            // Keep up to twice the target per denomination in reserve; only sweep
            // what is beyond that, so an ordinary spend doesn't drag a denomination's
            // surplus along until it is genuinely bloated.
            target_notes.extend(
                notes_amount
                    .iter()
                    .take(2 * TARGET_PER_DENOMINATION)
                    .cloned(),
            );

            for note in notes_amount.into_iter().skip(2 * TARGET_PER_DENOMINATION) {
                selected.push(note);
            }
        }

        let selected_value = selected.iter().map(|n| self.note_value(n)).sum();

        if excess_output <= selected_value {
            return Some(selected);
        }

        let mut last_note = None;

        for note in target_notes {
            let selected_value = selected.iter().map(|n| self.note_value(n)).sum();

            if self.note_value(&note) + selected_value <= excess_output {
                selected.push(note);
            } else {
                last_note = Some(note);
            }
        }

        selected.push(last_note?);

        Some(selected)
    }

    fn note_value(&self, note: &SpendableNote) -> Amount {
        note.amount()
            .checked_sub(self.cfg.input_fee)
            .expect("All our notes are economical")
    }

    fn select_output_denominations(
        output_fee: Amount,
        mut excess_input: Amount,
    ) -> Vec<Denomination> {
        let mut output_denominations = Vec::new();

        // Greedy binary representation of excess_input, largest->smallest.
        // For every tier except the largest, the descent ensures at most one
        // output per tier (since we only reach tier d once the remainder is
        // already below `denom(d+1) + output_fee`, and two of `denom(d)` cost
        // more than that). The largest tier absorbs whatever remains.
        for d in client_denominations().rev() {
            for _ in 0.. {
                match excess_input.checked_sub(d.amount() + output_fee) {
                    Some(remaining) => {
                        excess_input = remaining;
                        output_denominations.push(d);
                    }
                    None => break,
                }
            }
        }

        output_denominations
    }
}

impl MintClientModule {
    /// Count `account`'s `ECash` notes by denomination.
    pub fn get_count_by_denomination(&self, account: Account) -> BTreeMap<Denomination, u64> {
        let dbtx = self.client_ctx.db().begin_write();

        Self::get_count_by_denomination_dbtx(&dbtx, self.federation, account)
    }

    fn get_count_by_denomination_dbtx(
        dbtx: &impl picomint_redb::DbRead,
        federation: FederationId,
        account: Account,
    ) -> BTreeMap<Denomination, u64> {
        let mut acc = BTreeMap::new();

        for note in account_notes(dbtx, federation, account) {
            acc.entry(note.denomination)
                .and_modify(|count| *count += 1)
                .or_insert(1);
        }

        acc
    }

    /// Send `ECash` for the given amount. The
    /// amount will be rounded up to a multiple of 512 msat which is the
    /// smallest denomination used throughout the client. If the rounded
    /// amount cannot be covered with the ecash notes in the client's
    /// database the client will create a transaction to reissue the
    /// required denominations. It is safe to cancel the send method call
    /// before the reissue is complete in which case the reissued notes are
    /// returned to the regular balance. To cancel a successful ecash send
    /// simply receive it yourself.
    pub async fn send(&self, account: Account, amount: Amount) -> Result<ECash, SendECashError> {
        let amount = round_to_multiple(amount, client_denominations().next().unwrap().amount());

        let operation = OperationId::new_random();

        // Fast path: the account already has notes that sum exactly to
        // `amount`. Pull them out and emit `SendEvent` + `SendSuccessEvent`
        // atomically in one dbtx — no tx, no SM.
        let dbtx = self.client_ctx.db().begin_write();

        if let Some(ecash) = send_ecash_dbtx(&dbtx, self.federation, account, amount) {
            self.client_ctx
                .log_event(&dbtx, account, operation, SendEvent { amount });
            self.client_ctx.log_event(
                &dbtx,
                account,
                operation,
                SendSuccessEvent {
                    ecash: ecash.clone(),
                },
            );
            dbtx.commit();
            return Ok(ecash);
        }

        // Slow path: send_ecash_dbtx is read-only when it returns None,
        // so dropping this dbtx without committing is harmless.
        drop(dbtx);

        self.client_ctx
            .api()
            .liveness()
            .await
            .map_err(|_| SendECashError::Offline)?;

        let dbtx = self.client_ctx.db().begin_write();

        // Build target issuance requests up-front. Their outputs go into the
        // builder first; the balance loop then pulls funding from the wallet
        // and appends change outputs. We extend `issuance_requests` with the
        // change requests after balance so the order matches the transaction's
        // outputs and a single `MintStateMachine` can process both.
        let mut issuance_requests: Vec<NoteIssuanceRequest> = Vec::new();
        for d in represent_amount(amount) {
            let counter = self.next_counter(&dbtx, account);

            issuance_requests.push(NoteIssuanceRequest::new(account, d, counter, &self.secret));
        }

        let mut builder = TxBuilder::new();
        for request in &issuance_requests {
            builder.add_output(Output {
                output: wire::Output::Mint(request.output()),
                amount: request.denomination.amount(),
                fee: self.cfg.output_fee,
            });
        }

        // Between the targets and the change on both sides of the ledger:
        // in the builder's outputs, and in the issuance order that mirrors
        // them.
        let fee_requests = self.add_fee_outputs(&dbtx, account, &mut builder);

        let operator_fee = cut_into(&fee_requests, Account::OperatorFee);
        let integrator_fee = cut_into(&fee_requests, Account::IntegratorFee);

        issuance_requests.extend(fee_requests);

        let deficit = builder.deficit();

        let (funding_notes, change_requests) = self
            .balance(&dbtx, account, &mut builder)
            .ok_or(SendECashError::InsufficientBalance)?;

        let funding: Amount = funding_notes.iter().map(|n| n.amount()).sum();

        let remint = funding.saturating_sub(deficit);

        let tx_fee = builder.total_fee();
        let tx = builder.build();

        let txid = tx.compute_txid();

        // Everything past this point lands in the same dbtx that submits
        // the reissuance: SendEvent → RemintEvent → TxCreateEvent →
        // MintSM + SendSM. A crash before the commit leaves no half-state
        // behind; on restart the operation simply doesn't exist.
        self.tx_submission_executor.add_state_machine_dbtx(
            &dbtx,
            TxSubmissionStateMachine {
                account,
                operation,
                tx,
            },
        );

        self.client_ctx
            .log_event(&dbtx, account, operation, SendEvent { amount });

        self.client_ctx
            .log_event(&dbtx, account, operation, RemintEvent { txid });

        self.client_ctx.log_event(
            &dbtx,
            account,
            operation,
            crate::TxCreateEvent {
                txid,
                remint,
                tx_fee,
                operator_fee,
                integrator_fee,
            },
        );

        issuance_requests.extend(change_requests);

        let mint_sm = MintStateMachine {
            account,
            operation,
            spendable_notes: funding_notes,
            txid,
            issuance_requests,
        };

        self.mint_executor.add_state_machine_dbtx(&dbtx, mint_sm);

        let send_sm = SendStateMachine {
            account,
            operation,
            amount,
        };

        self.send_executor.add_state_machine_dbtx(&dbtx, send_sm);

        dbtx.commit();

        // Wait for the SendStateMachine to fire its terminal event on
        // the operation's event log.
        let mut stream = self.client_ctx.subscribe_operation_events(operation);
        while let Some(entry) = stream.next().await {
            if let Some(ev) = entry.to_event::<SendSuccessEvent>() {
                return Ok(ev.ecash);
            }
            if entry.to_event::<SendFailureEvent>().is_some() {
                return Err(SendECashError::Failure);
            }
        }
        unreachable!("subscribe_operation_events only ends at client shutdown")
    }

    /// Send everything `account` holds. `None` when it holds nothing.
    ///
    /// Takes the notes as they are rather than naming an amount, so there is
    /// no denomination to round to and no subset to find: always one dbtx,
    /// no transaction, no fee — and so, unlike `send`, not async.
    pub fn send_max(&self, account: Account) -> Option<ECash> {
        let operation = OperationId::new_random();
        let dbtx = self.client_ctx.db().begin_write();

        let notes = account_notes(&dbtx, self.federation, account);

        if notes.is_empty() {
            return None;
        }

        for note in &notes {
            dbtx.remove(&NoteTable(self.federation), &(account, note.clone()))
                .expect("Must delete existing spendable note");
        }

        let ecash = ECash::new(self.federation, notes);
        let amount = ecash.amount();

        self.client_ctx
            .log_event(&dbtx, account, operation, SendEvent { amount });
        self.client_ctx.log_event(
            &dbtx,
            account,
            operation,
            SendSuccessEvent {
                ecash: ecash.clone(),
            },
        );

        dbtx.commit();

        Some(ecash)
    }

    /// Receive the `ECash` into `account` by reissuing the notes.
    ///
    /// The [`OperationId`] is derived from the ecash bytes alone, and the
    /// guard it keys spans every account: a bundle can be reissued exactly
    /// once per federation, and a second attempt — into this account or the
    /// other one — fails with [`ReceiveECashError::AlreadyAttempted`] rather
    /// than submitting a transaction doomed against already-spent notes.
    pub fn receive(
        &self,
        account: Account,
        ecash: &ECash,
    ) -> Result<OperationId, ReceiveECashError> {
        let operation = OperationId::from_encodable(ecash);

        // A scan of a seed that never held anything produces one of these, so
        // this is the ordinary shape of an empty restore — not an edge case.
        // Without the guard it would balance to a transaction with no inputs
        // and no outputs and submit it.
        if ecash.notes.is_empty() {
            return Err(ReceiveECashError::Empty);
        }

        // Every note in the bundle is an input, and they are the only inputs
        // the account did not choose, so this is the one place a transaction
        // can be handed more of them than it may carry.
        if ecash.notes.len() > Transaction::MAX_INPUTS {
            return Err(ReceiveECashError::TooManyNotes);
        }

        if ecash.mint != self.federation {
            return Err(ReceiveECashError::WrongFederation);
        }

        if ecash
            .notes
            .iter()
            .any(|note| note.amount() <= self.cfg.input_fee)
        {
            return Err(ReceiveECashError::UneconomicalDenomination);
        }

        let mut tx_builder = TxBuilder::new();
        for note in &ecash.notes {
            tx_builder.add_input(Input {
                input: wire::Input::Mint(MintInput { note: note.note() }),
                keypair: note.keypair,
                amount: note.amount(),
                fee: self.cfg.input_fee,
            });
        }

        let dbtx = self.client_ctx.db().begin_write();

        if dbtx
            .insert(&ReceiveOperationTable(self.federation), &operation, &())
            .is_some()
        {
            return Err(ReceiveECashError::AlreadyAttempted);
        }

        let amount = ecash.amount();

        self.finalize_and_submit_tx(&dbtx, account, operation, tx_builder, |txid| ReceiveEvent {
            txid,
            amount,
        })
        .ok_or(ReceiveECashError::InsufficientFunds)?;

        dbtx.commit();

        Ok(operation)
    }

    fn remove_spendable_note(
        dbtx: &WriteTx,
        federation: FederationId,
        account: Account,
        spendable_note: &SpendableNote,
    ) {
        dbtx.remove(&NoteTable(federation), &(account, spendable_note.clone()))
            .expect("Must delete existing spendable note");
    }
}

/// Every note `account` holds. All accounts share one table, so this filters
/// on the key's leading [`Account`]; a wallet's note count is small enough
/// that walking the other accounts' rows costs nothing worth ranging over.
fn account_notes(
    dbtx: &impl picomint_redb::DbRead,
    federation: FederationId,
    account: Account,
) -> Vec<SpendableNote> {
    dbtx.iter(&NoteTable(federation), |r| {
        r.filter(|((a, _), ())| *a == account)
            .map(|((_, note), ())| note)
            .collect()
    })
}

/// Pull a set of `account`'s `SpendableNote`s whose denominations sum exactly
/// to `remaining_amount`, remove them, and return the resulting `ECash`.
/// Returns `None` if no exact-match combination exists. No events are logged
/// — callers do that.
fn send_ecash_dbtx(
    dbtx: &WriteTx,
    federation: FederationId,
    account: Account,
    mut remaining_amount: Amount,
) -> Option<ECash> {
    let mut sorted = account_notes(dbtx, federation, account);

    sorted.sort_by_key(|n| std::cmp::Reverse(n.denomination));

    let mut notes = vec![];

    for spendable_note in sorted {
        remaining_amount = match remaining_amount.checked_sub(spendable_note.amount()) {
            Some(amount) => amount,
            None => continue,
        };

        notes.push(spendable_note);
    }

    if remaining_amount != Amount::ZERO {
        return None;
    }

    for spendable_note in &notes {
        dbtx.remove(&NoteTable(federation), &(account, spendable_note.clone()))
            .expect("Must delete existing spendable note");
    }

    Some(ECash::new(federation, notes))
}

/// Drop every redb table this module owns under the caller's prefix.
/// Called by [`crate::Client::wipe`] for end-of-life client cleanup.
pub(crate) fn wipe_tables(dbtx: &WriteTx, federation: FederationId) {
    dbtx.delete_table(&NoteTable(federation));
    dbtx.delete_table(&ReceiveOperationTable(federation));
    dbtx.delete_table(&CounterTable(federation));
    dbtx.delete_table(&MintStateMachineTable(federation));
    dbtx.delete_table(&SendStateMachineTable(federation));
}

#[derive(Error, Debug, Clone, Eq, PartialEq)]
pub enum SendECashError {
    #[error("We need to reissue notes but the client is offline")]
    Offline,
    #[error("The clients balance is insufficient")]
    InsufficientBalance,
    #[error("A non-recoverable error has occurred")]
    Failure,
}

#[derive(Error, Debug, Clone, Eq, PartialEq)]
pub enum ReceiveECashError {
    #[error("The ECash bundle contains no notes")]
    Empty,
    #[error("The ECash bundle contains more notes than one transaction can reissue")]
    TooManyNotes,
    #[error("The ECash is from a different federation")]
    WrongFederation,
    #[error("ECash contains an uneconomical denomination")]
    UneconomicalDenomination,
    #[error("Receiving ecash requires additional funds")]
    InsufficientFunds,
    #[error("This ecash bundle has already been received")]
    AlreadyAttempted,
}

fn round_to_multiple(amount: Amount, min_denomiation: Amount) -> Amount {
    Amount::from_msat(amount.msat.next_multiple_of(min_denomiation.msat))
}

fn represent_amount(mut remaining_amount: Amount) -> Vec<Denomination> {
    let mut denominations = Vec::new();

    // Add denominations with a greedy algorithm
    for denomination in client_denominations().rev() {
        let n_add = remaining_amount / denomination.amount();

        denominations.extend(std::iter::repeat_n(denomination, n_add as usize));

        remaining_amount -= n_add * denomination.amount();
    }

    denominations
}
