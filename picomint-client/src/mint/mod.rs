pub use picomint_core::mint as common;

mod api;
mod client_db;
mod ecash;
mod events;
mod issuance;
mod mint_sm;
mod secret;
mod send_sm;

use picomint_redb::{Database, DbRead, ReadTx, WriteTx};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::Notify;

use crate::api::FederationApi;
use crate::client::Client;
use crate::context::ClientContext;
use crate::tx::{Input, Output, TxBuilder};
use crate::tx::{TxSubmissionStateMachine, TxSubmissionStateMachineTable};
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
use picomint_core::{Amount, TransactionId, wire};
use picomint_encoding::{Decodable, Encodable};
use tbs::aggregate_signature_shares;
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
/// [`largest_affordable_amount`] can leave — one whole-sat
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

impl SpendableNote {
    pub fn amount(&self) -> Amount {
        self.denomination.amount()
    }

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
/// is where it lands, in the dbtx [`crate::Client::add`] owns.
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
        &CounterTable,
        &(restore.federation, account),
        &restore.counter,
    );

    for note in &restore.notes {
        dbtx.insert(
            &NoteTable,
            &(restore.federation, account, note.clone()),
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
        let shares = api::signatures_restore(api, requests.clone(), cfg.tbs_pks.clone()).await;

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

        let nonces = candidates.iter().map(NoteIssuance::nonce).collect();

        let spent = api::spend_state(api, nonces).await;

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

        let issued = api::issuance_state(api, messages).await;

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

/// Hand out `account`'s next counter and persist the bump in the caller's
/// dbtx, so a counter is only spent once the transaction carrying its
/// blinded message is committed to.
fn next_counter(ctx: &ClientContext, dbtx: &WriteTx, account: Account) -> u64 {
    let counter = dbtx
        .get(&CounterTable, &(ctx.federation, account))
        .unwrap_or(0);

    dbtx.insert(&CounterTable, &(ctx.federation, account), &(counter + 1));

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
    ctx: &ClientContext,
    dbtx: &WriteTx,
    account: Account,
    builder: &mut TxBuilder,
) -> Vec<NoteIssuanceRequest> {
    // Gated on [`Account::User`] rather than on a list of the accounts to
    // skip, so an account added later is exempt by being what it is.
    if ctx.app_fee_ppm == 0 || !matches!(account, Account::User(_)) {
        return Vec::new();
    }

    let basis = builder.deficit() + builder.excess_input();

    let requests = fee_requests(ctx, dbtx, basis);

    for request in &requests {
        builder.add_output(Output {
            output: wire::Output::Mint(request.output()),
            amount: request.denomination.amount(),
            fee: ctx.config.mint.output_fee,
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
fn fee_requests(ctx: &ClientContext, dbtx: &WriteTx, basis: Amount) -> Vec<NoteIssuanceRequest> {
    let mut denominations = select_output_denominations(
        ctx.config.mint.output_fee,
        app_fee_cut(ctx, basis),
        client_denominations(),
    );

    // Sorted for the same reason the change outputs are: the shape of a
    // transaction's outputs should say as little as possible about which
    // of them are whose.
    denominations.sort();

    denominations
        .into_iter()
        .map(|d| {
            let counter = next_counter(ctx, dbtx, Account::AppFee);

            NoteIssuanceRequest::new(Account::AppFee, d, counter, &ctx.secret.mint_secret())
        })
        .collect()
}

fn app_fee_cut(ctx: &ClientContext, basis: Amount) -> Amount {
    Amount::from_msat(
        basis
            .msat
            .saturating_mul(ctx.app_fee_ppm)
            .saturating_div(1_000_000),
    )
}

/// Output value plus federation fees the integrator's cut adds to a
/// transaction from `account` whose caller outputs and their fees total
/// `basis`. Mirrors [`add_fee_outputs`] exactly, so
/// [`largest_affordable_amount`] can price the cut without building
/// a transaction.
fn app_fee_total(ctx: &ClientContext, account: Account, basis: Amount) -> Amount {
    if ctx.app_fee_ppm == 0 || !matches!(account, Account::User(_)) {
        return Amount::ZERO;
    }

    select_output_denominations(
        ctx.config.mint.output_fee,
        app_fee_cut(ctx, basis),
        client_denominations(),
    )
    .into_iter()
    .map(|d| d.amount() + ctx.config.mint.output_fee)
    .sum()
}

/// Balance the builder against mint's wallet (pulling funding notes when
/// underfunded, generating change outputs when overfunded), sign and
/// submit the resulting transaction, and spawn the
/// `MintStateMachine` that tracks the balance-side notes/requests
/// (if any).
///
/// `max` raises the change floor (see [`change_denominations`]): an
/// amount sized by [`largest_affordable_amount`] then pulls every
/// note the account holds and mints no change at all, so a committed
/// submission leaves the account empty.
///
/// `targets` are issuance requests whose outputs the caller already added
/// to the builder, ahead of everything this method adds. They are prepended
/// to the state machine's request list, which must mirror the transaction's
/// mint-output order — the signature shares come back indexed by it.
///
/// `event` builds the module's initiating event (e.g. `SendEvent`)
/// from the txid; this method logs it before the bookkeeping
/// `TxCreateEvent` so the operation's event log opens with the
/// module event.
#[allow(clippy::too_many_arguments)]
pub(crate) fn finalize_and_submit_tx<E: crate::eventlog::Event + Send>(
    ctx: &ClientContext,
    dbtx: &WriteTx,
    account: Account,
    operation: OperationId,
    mut builder: TxBuilder,
    targets: Vec<NoteIssuanceRequest>,
    max: bool,
    event: impl FnOnce(TransactionId) -> E,
) -> Option<TransactionId> {
    // Ahead of the deficit the funding has to cover, so the cut is funded
    // like any other output rather than out of the change.
    let fee_requests = add_fee_outputs(ctx, dbtx, account, &mut builder);

    let app_fee = fee_requests.iter().map(|r| r.denomination.amount()).sum();

    let mut issuance_requests = targets;

    issuance_requests.extend(fee_requests);

    let deficit = builder.deficit();

    let (spendable_notes, change_requests) = fund(ctx, dbtx, account, &mut builder, max)?;

    issuance_requests.extend(change_requests);

    let funding: Amount = spendable_notes.iter().map(|n| n.amount()).sum();

    let remint = funding.saturating_sub(deficit);

    let txid = submit(
        ctx, dbtx, account, operation, builder, remint, app_fee, event,
    );

    if !spendable_notes.is_empty() || !issuance_requests.is_empty() {
        let sm = MintStateMachine {
            account,
            operation,
            spendable_notes,
            txid,
            issuance_requests,
        };
        crate::executor::add_state_machine_dbtx(ctx, MintStateMachineTable, dbtx, sm);
    }

    Some(txid)
}

/// Mint-side transaction balancing. Pulls funding notes from `account`
/// when the builder is underfunded, then absorbs any excess as change
/// outputs issued back to the same account. Sub-denomination dust below
/// `smallest_change_denom + output_fee` is left as implicit federation
/// revenue. Returns `None` iff the account holds insufficient funds to
/// cover the builder's deficit, which is the only way balancing fails.
fn fund(
    ctx: &ClientContext,
    dbtx: &WriteTx,
    account: Account,
    builder: &mut TxBuilder,
    max: bool,
) -> Option<(Vec<SpendableNote>, Vec<NoteIssuanceRequest>)> {
    let mut spendable_notes = select_funding_input(ctx, dbtx, account, builder.deficit())?;

    // Sort by denomination to minimize information leaked about
    // which notes the wallet held.
    spendable_notes.sort_by_key(|note| note.denomination);

    for note in &spendable_notes {
        remove_spendable_note(dbtx, ctx.federation, account, note);
        builder.add_input(Input {
            input: wire::Input::Mint(MintInput { note: note.note() }),
            keypair: note.keypair,
            amount: note.amount(),
            fee: ctx.config.mint.input_fee,
        });
    }

    assert_eq!(builder.deficit(), Amount::ZERO);

    let mut denoms = select_output_denominations(
        ctx.config.mint.output_fee,
        builder.excess_input(),
        change_denominations(max),
    );

    // Sort to minimize information leaked about the change shape.
    denoms.sort();

    let mut issuance_requests = Vec::new();

    for d in denoms {
        let counter = next_counter(ctx, dbtx, account);

        issuance_requests.push(NoteIssuanceRequest::new(
            account,
            d,
            counter,
            &ctx.secret.mint_secret(),
        ));
    }

    for request in &issuance_requests {
        builder.add_output(Output {
            output: wire::Output::Mint(request.output()),
            amount: request.denomination.amount(),
            fee: ctx.config.mint.output_fee,
        });
    }

    assert_eq!(builder.deficit(), Amount::ZERO);

    Some((spendable_notes, issuance_requests))
}

/// Sign the builder, spawn the `TxSubmissionStateMachine`, log the
/// caller's `event` followed by `TxCreateEvent`.
#[allow(clippy::too_many_arguments)]
fn submit<E: crate::eventlog::Event + Send>(
    ctx: &ClientContext,
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

    crate::executor::add_state_machine_dbtx(ctx, TxSubmissionStateMachineTable, dbtx, sm);

    ctx.log_event(dbtx, account, operation, event(txid));

    ctx.log_event(
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

/// Value `account`'s notes can deliver to a transaction's outputs when
/// spent in full — their face value minus one input fee per note. The
/// budget a max-send amount is solved against.
fn max_spendable(ctx: &ClientContext, account: Account) -> Amount {
    account_notes(&ctx.db.begin_read(), ctx.federation, account)
        .iter()
        .map(|note| note_value(ctx, note))
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
/// is priced in here, mirroring [`add_fee_outputs`], so a rail
/// cannot size against a different cut than the one it will pay.
///
/// Whole sats because that is the granularity every rail's amount entry
/// works in.
pub(crate) fn largest_affordable_amount(
    ctx: &ClientContext,
    account: Account,
    rail_fees: impl Fn(Amount) -> Amount,
) -> Amount {
    let spendable = max_spendable(ctx, account);

    let total = |amount: Amount| {
        let basis = amount + rail_fees(amount);

        basis + app_fee_total(ctx, account, basis)
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
    ctx: &ClientContext,
    dbtx: &WriteTx,
    account: Account,
    excess_output: Amount,
) -> Option<Vec<SpendableNote>> {
    let mut selected = Vec::new();
    let mut target_notes = Vec::new();

    let all_notes = account_notes(dbtx, ctx.federation, account);

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

    let selected_value = selected.iter().map(|n| note_value(ctx, n)).sum();

    if excess_output <= selected_value {
        return Some(selected);
    }

    let mut last_note = None;

    for note in target_notes {
        let selected_value = selected.iter().map(|n| note_value(ctx, n)).sum();

        if note_value(ctx, &note) + selected_value <= excess_output {
            selected.push(note);
        } else {
            last_note = Some(note);
        }
    }

    selected.push(last_note?);

    Some(selected)
}

fn note_value(ctx: &ClientContext, note: &SpendableNote) -> Amount {
    note.amount()
        .checked_sub(ctx.config.mint.input_fee)
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

fn get_count_by_denomination_dbtx(
    dbtx: &impl DbRead,
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

fn remove_spendable_note(
    dbtx: &WriteTx,
    federation: FederationId,
    account: Account,
    spendable_note: &SpendableNote,
) {
    dbtx.remove(&NoteTable, &(federation, account, spendable_note.clone()))
        .expect("Must delete existing spendable note");
}

/// Every note `account` holds — an indexed prefix scan over the key's
/// leading `(federation, account)` columns.
fn account_notes(
    dbtx: &impl DbRead,
    federation: FederationId,
    account: Account,
) -> Vec<SpendableNote> {
    dbtx.prefix(&NoteTable, &(federation, account), |r| {
        r.map(|entry| entry.0.2).collect()
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
        remove_spendable_note(dbtx, federation, account, spendable_note);
    }

    Some(ECash::new(federation, notes))
}

/// Remove every row this module owns under the caller's federation prefix.
/// Called by [`crate::Client::remove`] for end-of-life cleanup.
pub(crate) fn wipe_tables(dbtx: &WriteTx, federation: FederationId) {
    dbtx.remove_prefix(&NoteTable, &federation);
    dbtx.remove_prefix(&ReceiveOperationTable, &federation);
    dbtx.remove_prefix(&CounterTable, &federation);
    dbtx.remove_prefix(&MintStateMachineTable, &federation);
    dbtx.remove_prefix(&SendStateMachineTable, &federation);
}

/// Whether any of this module's state machines for `operation` is still
/// active under `federation`.
pub(crate) fn operation_is_active(
    dbtx: &ReadTx,
    federation: FederationId,
    operation: OperationId,
) -> bool {
    dbtx.prefix(&MintStateMachineTable, &federation, |r| {
        r.any(|entry| entry.1.operation == operation)
    }) || dbtx.prefix(&SendStateMachineTable, &federation, |r| {
        r.any(|entry| entry.1.operation == operation)
    })
}

/// Notify handles for this module's state machine tables, fired on every
/// commit that writes them.
pub(crate) fn sm_notifies(db: &Database) -> Vec<Arc<Notify>> {
    vec![
        db.notify_for_table(&MintStateMachineTable),
        db.notify_for_table(&SendStateMachineTable),
    ]
}

/// Resume this federation's persisted mint state machines. Called exactly
/// once, at federation bring-up.
pub(crate) fn resume(ctx: &ClientContext) {
    crate::executor::resume::<TxSubmissionStateMachine, _>(ctx, TxSubmissionStateMachineTable);

    crate::executor::resume::<MintStateMachine, _>(ctx, MintStateMachineTable);

    crate::executor::resume::<SendStateMachine, _>(ctx, SendStateMachineTable);
}

#[derive(Error, Debug, Clone, Eq, PartialEq)]
pub enum SendECashError {
    #[error("We need to reissue notes but the client is offline")]
    Offline,
    #[error("The clients balance is insufficient")]
    InsufficientBalance,
    #[error("A non-recoverable error has occurred")]
    Failure,
    #[error("Federation is not joined")]
    NotJoined,
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
    #[error("Federation is not joined")]
    NotJoined,
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

// ─── Flat federation-keyed surface ───────────────────────────────────────

/// `account`'s ecash balance: the face value of every note it holds.
pub(crate) fn balance(dbtx: &impl DbRead, federation: FederationId, account: Account) -> Amount {
    account_notes(dbtx, federation, account)
        .iter()
        .map(|note| note.amount())
        .sum()
}

impl Client {
    /// `account`'s ecash balance. Pure read — never brings the federation
    /// up, and an unjoined federation simply holds nothing.
    pub fn mint_balance(&self, federation: FederationId, account: Account) -> Amount {
        balance(&self.db.begin_read(), federation, account)
    }

    /// Yields `account`'s balance whenever the note table is written. The
    /// same value may be yielded repeatedly — every federation's accounts
    /// share one table, so writes to another account or federation wake this
    /// stream too. Pure read — never brings the federation up.
    pub fn mint_subscribe_balance(
        &self,
        federation: FederationId,
        account: Account,
    ) -> futures::stream::BoxStream<'static, Amount> {
        let notify = self.db.notify_for_table(&NoteTable);
        let db = self.db.clone();

        Box::pin(async_stream::stream! {
            loop {
                // Registered before the read so a write landing in between
                // still wakes the already-registered waiter.
                let notified = notify.notified();

                yield balance(&db.begin_read(), federation, account);

                notified.await;
            }
        })
    }

    /// Count `account`'s notes by denomination. Pure read — never brings
    /// the federation up.
    pub fn mint_count_by_denomination(
        &self,
        federation: FederationId,
        account: Account,
    ) -> BTreeMap<Denomination, u64> {
        get_count_by_denomination_dbtx(&self.db.begin_read(), federation, account)
    }

    /// Send [`ECash`] for the given amount from `account`. The amount is
    /// rounded up to a multiple of the smallest client denomination; when the
    /// balance's denominations cannot cover it exactly, a reissue transaction
    /// mints them first. Safe to cancel before the reissue completes — the
    /// reissued notes return to the regular balance. To cancel a successful
    /// send, receive the ecash yourself.
    pub async fn mint_send(
        &self,
        federation: FederationId,
        account: Account,
        amount: Amount,
    ) -> Result<ECash, SendECashError> {
        let ctx = self
            .ctx(federation)
            .map_err(|_| SendECashError::NotJoined)?;

        let amount = round_to_multiple(
            amount,
            client_denominations()
                .next()
                .expect("There is at least one client denomination")
                .amount(),
        );

        let operation = OperationId::new_random();

        // Fast path: the account already has notes that sum exactly to
        // `amount`. Pull them out and emit `SendEvent` + `SendSuccessEvent`
        // atomically in one dbtx — no tx, no SM.
        let dbtx = ctx.db.begin_write();

        if let Some(ecash) = send_ecash_dbtx(&dbtx, ctx.federation, account, amount) {
            ctx.log_event(&dbtx, account, operation, SendEvent { amount });
            ctx.log_event(
                &dbtx,
                account,
                operation,
                SendSuccessEvent {
                    ecash: ecash.to_string(),
                },
            );
            dbtx.commit();
            return Ok(ecash);
        }

        // Slow path: send_ecash_dbtx is read-only when it returns None,
        // so dropping this dbtx without committing is harmless.
        drop(dbtx);

        crate::api::liveness(&ctx.api)
            .await
            .map_err(|_| SendECashError::Offline)?;

        let dbtx = ctx.db.begin_write();

        // Target issuance requests up-front; their outputs go into the
        // builder first, so the funding and change the finalize call adds
        // land behind them.
        let targets: Vec<NoteIssuanceRequest> = represent_amount(amount)
            .into_iter()
            .map(|d| {
                let counter = next_counter(&ctx, &dbtx, account);

                NoteIssuanceRequest::new(account, d, counter, &ctx.secret.mint_secret())
            })
            .collect();

        let mut builder = TxBuilder::new();

        for request in &targets {
            builder.add_output(Output {
                output: wire::Output::Mint(request.output()),
                amount: request.denomination.amount(),
                fee: ctx.config.mint.output_fee,
            });
        }

        // Everything below lands in the same dbtx that submits the
        // reissuance: SendEvent → RemintEvent → TxCreateEvent →
        // MintSM + SendSM. A crash before the commit leaves no half-state
        // behind; on restart the operation simply doesn't exist.
        ctx.log_event(&dbtx, account, operation, SendEvent { amount });

        finalize_and_submit_tx(
            &ctx,
            &dbtx,
            account,
            operation,
            builder,
            targets,
            false,
            |txid| RemintEvent { txid },
        )
        .ok_or(SendECashError::InsufficientBalance)?;

        let send_sm = SendStateMachine {
            account,
            operation,
            amount,
        };

        crate::executor::add_state_machine_dbtx(&ctx, SendStateMachineTable, &dbtx, send_sm);

        dbtx.commit();

        // Wait for the SendStateMachine to fire its terminal event on
        // the operation's event log.
        let mut stream = ctx.subscribe_operation_events(operation);
        while let Some(entry) = stream.next().await {
            if let Some(ev) = entry.to_event::<SendSuccessEvent>() {
                return ev
                    .ecash
                    .parse()
                    .map(Ok)
                    .expect("logged ecash is its own to_string, which from_str reverses");
            }
            if entry.to_event::<SendFailureEvent>().is_some() {
                return Err(SendECashError::Failure);
            }
        }
        unreachable!("subscribe_operation_events only ends at client shutdown")
    }

    /// Send everything `account` holds as one [`ECash`] bundle. `None` when
    /// it holds nothing.
    pub fn mint_send_max(
        &self,
        federation: FederationId,
        account: Account,
    ) -> anyhow::Result<Option<ECash>> {
        let ctx = self.ctx(federation)?;

        let operation = OperationId::new_random();
        let dbtx = ctx.db.begin_write();

        let notes = account_notes(&dbtx, ctx.federation, account);

        if notes.is_empty() {
            return Ok(None);
        }

        for note in &notes {
            remove_spendable_note(&dbtx, ctx.federation, account, note);
        }

        let ecash = ECash::new(ctx.federation, notes);
        let amount = ecash.amount();

        ctx.log_event(&dbtx, account, operation, SendEvent { amount });
        ctx.log_event(
            &dbtx,
            account,
            operation,
            SendSuccessEvent {
                ecash: ecash.to_string(),
            },
        );

        dbtx.commit();

        Ok(Some(ecash))
    }

    /// Receive an [`ECash`] bundle into `account` by reissuing its notes.
    /// A bundle can be received exactly once per federation.
    pub fn mint_receive(
        &self,
        federation: FederationId,
        account: Account,
        ecash: &ECash,
    ) -> Result<OperationId, ReceiveECashError> {
        let ctx = self
            .ctx(federation)
            .map_err(|_| ReceiveECashError::NotJoined)?;

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

        if ecash.mint != ctx.federation {
            return Err(ReceiveECashError::WrongFederation);
        }

        if ecash
            .notes
            .iter()
            .any(|note| note.amount() <= ctx.config.mint.input_fee)
        {
            return Err(ReceiveECashError::UneconomicalDenomination);
        }

        let mut tx_builder = TxBuilder::new();
        for note in &ecash.notes {
            tx_builder.add_input(Input {
                input: wire::Input::Mint(MintInput { note: note.note() }),
                keypair: note.keypair,
                amount: note.amount(),
                fee: ctx.config.mint.input_fee,
            });
        }

        let dbtx = ctx.db.begin_write();

        if dbtx
            .insert(&ReceiveOperationTable, &(ctx.federation, operation), &())
            .is_some()
        {
            return Err(ReceiveECashError::AlreadyAttempted);
        }

        let amount = ecash.amount();

        finalize_and_submit_tx(
            &ctx,
            &dbtx,
            account,
            operation,
            tx_builder,
            Vec::new(),
            false,
            |txid| ReceiveEvent { txid, amount },
        )
        .ok_or(ReceiveECashError::InsufficientFunds)?;

        dbtx.commit();

        Ok(operation)
    }
}

impl Client {
    /// Fund, sign and submit a hand-built transaction, minting change into
    /// `account`. An escape hatch for integration tests that forge foreign
    /// inputs; not part of the supported surface.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn mint_finalize_and_submit_tx<E: crate::eventlog::Event + Send>(
        &self,
        federation: FederationId,
        dbtx: &WriteTx,
        account: Account,
        operation: OperationId,
        tx_builder: TxBuilder,
        max: bool,
        event: impl FnOnce(TransactionId) -> E,
    ) -> anyhow::Result<Option<TransactionId>> {
        let ctx = self.ctx(federation)?;

        Ok(finalize_and_submit_tx(
            &ctx,
            dbtx,
            account,
            operation,
            tx_builder,
            Vec::new(),
            max,
            event,
        ))
    }
}
