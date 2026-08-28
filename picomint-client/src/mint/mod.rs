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

/// Denominations change may be minted in. An ordinary transaction uses all
/// of them; a max send skips the two smallest, which lifts the change
/// threshold (smallest change denomination + output fee = 2148 msat) above
/// the largest sub-sat remainder an amount sized by
/// [`MintClientModule::largest_affordable_amount`] can leave — one whole-sat
/// pricing step plus the gateway fee limits' ppm rates and the app cut's
/// dust wrap, ~1.65 sat. A freshly priced max send therefore pulls every
/// note and mints no change at all, while an amount gone stale against a
/// moved balance falls back to minting change like any other send.
fn change_denominations(max: bool) -> impl DoubleEndedIterator<Item = Denomination> {
    client_denominations().skip(if max { 2 } else { 0 })
}

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
/// Produced by [`scan`], which touches no database at all — [`commit_scan`]
/// is where it lands, in the dbtx [`crate::Join::commit`] was handed.
#[derive(Debug, Clone)]
pub(crate) struct Restore {
    federation: FederationId,
    notes: Vec<SpendableNote>,
    counter: u64,
}

/// Persist what a [`scan`] of `account` found, in a dbtx the caller owns.
///
/// The counter mark must land before the account issues anything: an account
/// resuming from zero would re-derive nonces the federation has already
/// signed, and every note behind them would be stranded. That is why this
/// shares a dbtx with whatever marks the federation as joined — a crash
/// leaves either both or neither.
///
/// The notes go straight into the wallet rather than through a reissuance
/// first, so the balance is simply there when the client opens. The federation
/// was asked about each of these nonces by name during the scan and can
/// recognise them when they are spent — a restored wallet is linkable to its
/// scan until the notes churn out through the change of ordinary
/// transactions. Trading them in up front would close that, at the cost of a
/// transaction bounded by [`Transaction::MAX_INPUTS`], which a wallet holding
/// more notes than that could not be restored through at all.
pub(crate) fn commit_scan(dbtx: &WriteTx, account: Account, restore: &Restore) {
    dbtx.insert(
        &CounterTable(restore.federation),
        &account,
        &restore.counter,
    );

    for note in &restore.notes {
        dbtx.insert(
            &NoteTable(restore.federation),
            &(account, note.clone()),
            &(),
        );
    }
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

    /// Blinded outputs paying the integrator's cut of the value a transaction
    /// moves, and the issuance requests that redeem them into
    /// [`Account::AppFee`].
    ///
    /// Called on a builder holding its caller's outputs and no funding, where
    /// the imbalance is still the operation's own amount — a payment's
    /// outputs on the way out, a claim's inputs on the way in, and exactly one
    /// of the two. A transaction that moves nothing pays nothing.
    ///
    /// Charged on transactions funded from a user's balance and on nothing
    /// else. Spending the fee account is how the integrator collects, and a
    /// collection pays no cut — it would leave a remainder that could never
    /// be swept.
    fn add_fee_outputs(
        &self,
        dbtx: &WriteTx,
        account: Account,
        builder: &mut TxBuilder,
    ) -> Vec<NoteIssuanceRequest> {
        // Gated on [`Account::User`] rather than on a list of the accounts to
        // skip, so an account added later is exempt by being what it is.
        if self.app_fee_ppm == 0 || !matches!(account, Account::User(_)) {
            return Vec::new();
        }

        let basis = builder.deficit() + builder.excess_input();

        let requests = self.fee_requests(dbtx, basis);

        for request in &requests {
            builder.add_output(Output {
                output: wire::Output::Mint(request.output()),
                amount: request.denomination.amount(),
                fee: self.cfg.output_fee,
            });
        }

        requests
    }

    /// The cut's issuance requests, in denominations totalling the
    /// configured parts per million of `basis`.
    ///
    /// The cut absorbs the federation's fee on the outputs carrying it, so
    /// what is charged is what is charged; a cut too small to buy the
    /// smallest denomination buys nothing at all and is left behind as
    /// federation revenue, the same way change dust is.
    fn fee_requests(&self, dbtx: &WriteTx, basis: Amount) -> Vec<NoteIssuanceRequest> {
        let mut denominations = Self::select_output_denominations(
            self.cfg.output_fee,
            self.app_fee_cut(basis),
            client_denominations(),
        );

        // Sorted for the same reason the change outputs are: the shape of a
        // transaction's outputs should say as little as possible about which
        // of them are whose.
        denominations.sort();

        denominations
            .into_iter()
            .map(|d| {
                let counter = self.next_counter(dbtx, Account::AppFee);

                NoteIssuanceRequest::new(Account::AppFee, d, counter, &self.secret)
            })
            .collect()
    }

    fn app_fee_cut(&self, basis: Amount) -> Amount {
        Amount::from_msat(
            basis
                .msat
                .saturating_mul(self.app_fee_ppm)
                .saturating_div(1_000_000),
        )
    }

    /// Output value plus federation fees the integrator's cut adds to a
    /// transaction from `account` whose caller outputs and their fees total
    /// `basis`. Mirrors [`Self::add_fee_outputs`] exactly, so
    /// [`Self::largest_affordable_amount`] can price the cut without building
    /// a transaction.
    fn app_fee_total(&self, account: Account, basis: Amount) -> Amount {
        if self.app_fee_ppm == 0 || !matches!(account, Account::User(_)) {
            return Amount::ZERO;
        }

        Self::select_output_denominations(
            self.cfg.output_fee,
            self.app_fee_cut(basis),
            client_denominations(),
        )
        .into_iter()
        .map(|d| d.amount() + self.cfg.output_fee)
        .sum()
    }
}

impl MintClientModule {
    pub fn new(
        federation: FederationId,
        cfg: MintConfigConsensus,
        context: ClientContext,
        secret: MintSecret,
        app_fee_ppm: u64,
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
            app_fee_ppm,
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
    /// pays into [`Account::AppFee`]. Zero for a client that charges nothing,
    /// which is every client the workspace itself builds.
    app_fee_ppm: u64,
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
    /// `max` raises the change floor (see [`change_denominations`]): an
    /// amount sized by [`Self::largest_affordable_amount`] then pulls every
    /// note the account holds and mints no change at all, so a committed
    /// submission leaves the account empty.
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
        max: bool,
        event: impl FnOnce(TransactionId) -> E,
    ) -> Option<TransactionId> {
        // Ahead of the deficit the funding has to cover, so the cut is funded
        // like any other output rather than out of the change.
        let mut issuance_requests = self.add_fee_outputs(dbtx, account, &mut builder);

        let app_fee = issuance_requests
            .iter()
            .map(|r| r.denomination.amount())
            .sum();

        let deficit = builder.deficit();

        let (spendable_notes, change_requests) = self.balance(dbtx, account, &mut builder, max)?;

        issuance_requests.extend(change_requests);

        let funding: Amount = spendable_notes.iter().map(|n| n.amount()).sum();

        let remint = funding.saturating_sub(deficit);

        let txid = self.submit(dbtx, account, operation, builder, remint, app_fee, event);

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
    /// `smallest_change_denom + output_fee` is left as implicit federation
    /// revenue. Returns `None` iff the account holds insufficient funds to
    /// cover the builder's deficit, which is the only way balancing fails.
    fn balance(
        &self,
        dbtx: &WriteTx,
        account: Account,
        builder: &mut TxBuilder,
        max: bool,
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

        let mut denoms = Self::select_output_denominations(
            self.cfg.output_fee,
            builder.excess_input(),
            change_denominations(max),
        );

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
        app_fee: Amount,
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
                app_fee,
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

    /// Value `account`'s notes can deliver to a transaction's outputs when
    /// spent in full — their face value minus one input fee per note. The
    /// budget a max-send amount is solved against.
    fn max_spendable(&self, account: Account) -> Amount {
        account_notes(&self.client_ctx.db().begin_read(), self.federation, account)
            .iter()
            .map(|note| self.note_value(note))
            .sum()
    }

    /// Largest whole-sat amount `account`'s notes can deliver to a rail's
    /// outputs when spent in full: a max send for this amount pulls every
    /// note and mints no change (see [`change_denominations`]), donating the
    /// sub-sat remainder to the federation the way change dust already is.
    /// Zero when even the fees on a zero-amount payment do not fit.
    ///
    /// `rail_fees` prices everything the rail's outputs add on top of the
    /// amount itself — the rail's own fees and the federation's fee on the
    /// outputs that carry them — and must be monotone. The integrator's cut
    /// is priced in here, mirroring [`Self::add_fee_outputs`], so a rail
    /// cannot size against a different cut than the one it will pay.
    ///
    /// Whole sats because that is the granularity every rail's amount entry
    /// works in.
    pub fn largest_affordable_amount(
        &self,
        account: Account,
        rail_fees: impl Fn(Amount) -> Amount,
    ) -> Amount {
        let spendable = self.max_spendable(account);

        let total = |amount: Amount| {
            let basis = amount + rail_fees(amount);

            basis + self.app_fee_total(account, basis)
        };

        if spendable < total(Amount::ZERO) {
            return Amount::ZERO;
        }

        let mut lo = 0;
        let mut hi = spendable.msat / 1000;

        while lo < hi {
            let mid = (lo + hi).div_ceil(2);

            if total(Amount::from_sat(mid)) <= spendable {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }

        Amount::from_sat(lo)
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
        denominations: impl DoubleEndedIterator<Item = Denomination>,
    ) -> Vec<Denomination> {
        let mut output_denominations = Vec::new();

        // Greedy binary representation of excess_input, largest->smallest.
        // For every tier except the largest, the descent ensures at most one
        // output per tier (since we only reach tier d once the remainder is
        // already below `denom(d+1) + output_fee`, and two of `denom(d)` cost
        // more than that). The largest tier absorbs whatever remains.
        for d in denominations.rev() {
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

        let app_fee = fee_requests.iter().map(|r| r.denomination.amount()).sum();

        issuance_requests.extend(fee_requests);

        let deficit = builder.deficit();

        let (funding_notes, change_requests) = self
            .balance(&dbtx, account, &mut builder, false)
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
                app_fee,
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

        self.finalize_and_submit_tx(&dbtx, account, operation, tx_builder, false, |txid| {
            ReceiveEvent { txid, amount }
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
