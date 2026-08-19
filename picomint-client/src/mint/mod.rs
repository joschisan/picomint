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
use crate::tx::{
    Transaction, TxSubmissionSmContext, TxSubmissionStateMachine, TxSubmissionStateMachineTable,
};
use anyhow::ensure;
use client_db::{CounterTable, NoteTable, ReceiveOperationTable};
pub use events::*;
use futures::StreamExt;
use picomint_core::config::FederationId;
use picomint_core::core::OperationId;
use picomint_core::mint::config::{MintConfigConsensus, client_denominations};
use picomint_core::mint::{Denomination, MintInput, Note};
use picomint_core::secp256k1::{Keypair, XOnlyPublicKey};
use picomint_core::{Amount, PeerId, TransactionId, wire};
use picomint_encoding::{Decodable, Encodable};
use tbs::{AggregatePublicKey, aggregate_signature_shares};
use thiserror::Error;

pub use self::ecash::ECash;
use self::issuance::NoteIssuanceRequest;
use self::mint_sm::{MintStateMachine, MintStateMachineTable};
pub use self::secret::MintSecret;
use self::send_sm::{SendStateMachine, SendStateMachineTable};

const TARGET_PER_DENOMINATION: usize = 3;

/// Counters probed per round trip, per denomination — and, since a scan stops
/// on its first fully empty batch, the gap limit itself.
///
/// A transaction emits at most two outputs of any one denomination
/// (`represent_amount` leaves a remainder below the next tier down, and
/// `select_output_denominations` is one-per-tier), so a batch this size
/// tolerates thirty-odd consecutive failed transactions burning counters
/// before a live note could be stranded beyond the gap.
const RECOVERY_BATCH: u64 = 64;

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

/// Everything a seed-only scan of the federation turned up: the notes the
/// wallet still owns, and how far each denomination's counter space was
/// walked.
///
/// Produced by [`crate::recover`], which touches no database at all, and
/// applied by [`MintClientModule::commit_recovery`] inside a dbtx the caller
/// owns. Keeping the two apart is what makes joining atomic: an integrator
/// commits the recovered wallet alongside their own bookkeeping in one write,
/// and a crash before that write leaves nothing half-restored to detect.
#[derive(Debug, Clone)]
pub struct Recovery {
    federation: FederationId,
    notes: Vec<SpendableNote>,
    counters: BTreeMap<Denomination, u64>,
}

impl Recovery {
    /// Gross value recovered, before the reissuance's fees.
    pub fn amount(&self) -> Amount {
        self.notes.iter().map(SpendableNote::amount).sum()
    }

    pub fn federation(&self) -> FederationId {
        self.federation
    }
}

/// Rebuild a wallet's notes from its seed, without touching a database.
///
/// Runs in two phases. Every denomination owns an independent counter space,
/// so all of them are scanned concurrently, each stopping as soon as one full
/// batch of counters turns up nothing at all — neither a nonce the federation
/// has seen spent nor a blinded message it ever signed. Only once the live set
/// has settled are the signature shares fetched, in a single request.
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
) -> anyhow::Result<Recovery> {
    // Each scan carries its own denomination back out rather than relying on
    // `join_all` order: writing one denomination's counter into another's slot
    // would hand out counters the federation has already signed.
    let scans = client_denominations().map(|denomination| async move {
        (
            denomination,
            scan_denomination(api, secret, denomination).await,
        )
    });

    let scanned = futures::future::join_all(scans).await;

    let counters = scanned
        .iter()
        .map(|(denomination, (counter, _))| (*denomination, *counter))
        .collect();

    let requests: Vec<NoteIssuanceRequest> = scanned
        .iter()
        .flat_map(|(_, (_, found))| found)
        .cloned()
        .collect();

    let mut notes = Vec::with_capacity(requests.len());

    if !requests.is_empty() {
        let shares = api
            .signature_shares_recovery(requests.clone(), cfg.tbs_pks.clone())
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
                "Recovered note failed verification against the aggregate public key"
            );

            notes.push(note);
        }
    }

    Ok(Recovery {
        federation,
        notes,
        counters,
    })
}

/// Walk one denomination's counter space. Returns the counter the scan reached
/// and the candidates the federation both signed and has not seen spent — the
/// notes still live, whose shares the caller fetches in bulk.
async fn scan_denomination(
    api: &FederationApi,
    secret: &MintSecret,
    denomination: Denomination,
) -> (u64, Vec<NoteIssuanceRequest>) {
    let mut found = Vec::new();
    let mut counter = 0;

    loop {
        let candidates: Vec<NoteIssuanceRequest> = (counter..counter + RECOVERY_BATCH)
            .map(|c| NoteIssuanceRequest::new(denomination, c, secret))
            .collect();

        let spent = api
            .spend_state(candidates.iter().map(NoteIssuanceRequest::nonce).collect())
            .await;

        // Deriving a blinded message costs some twenty times what the nonce
        // did, so it is paid only for counters that survived the spend check.
        let unspent: Vec<NoteIssuanceRequest> = candidates
            .into_iter()
            .zip(&spent)
            .filter(|(_, spent)| !**spent)
            .map(|(candidate, _)| candidate)
            .collect();

        let messages = tokio::task::spawn_blocking({
            let unspent = unspent.clone();

            move || {
                unspent
                    .iter()
                    .map(NoteIssuanceRequest::blinded_message)
                    .collect()
            }
        })
        .await
        .expect("Blinded message derivation cannot panic");

        let issued = api.issuance_state(messages).await;

        counter += RECOVERY_BATCH;

        if !issued.contains(&true) && !spent.contains(&true) {
            return (counter, found);
        }

        found.extend(
            unspent
                .into_iter()
                .zip(&issued)
                .filter(|(_, issued)| **issued)
                .map(|(candidate, _)| candidate),
        );
    }
}

impl MintClientModule {
    /// Apply a [`Recovery`] in the caller's dbtx: persist the counters the
    /// scan reached, then sweep the recovered notes into a single reissuance
    /// transaction and log the terminal [`RecoveryEvent`] under the returned
    /// operation id.
    ///
    /// Nothing is written until the caller commits, so this belongs in the
    /// same dbtx as whatever marks the federation as joined. From
    /// `TxAcceptEvent` on, the operation rides the standard mint state
    /// machines.
    ///
    /// The counters matter as much as the notes: a restored wallet resuming
    /// from zero would re-derive nonces the federation has already signed.
    pub fn commit_recovery(&self, dbtx: &WriteTx, recovery: &Recovery) -> OperationId {
        assert_eq!(
            recovery.federation, self.federation,
            "Recovery belongs to a different federation",
        );

        let operation = OperationId::new_random();
        let amount = recovery.amount();

        // Ahead of the sweep below, which allocates change counters of its own.
        for (denomination, counter) in &recovery.counters {
            dbtx.insert(&CounterTable(self.federation), denomination, counter);
        }

        if recovery.notes.is_empty() {
            self.client_ctx.log_event(
                dbtx,
                operation,
                events::RecoveryEvent { amount, txid: None },
            );

            return operation;
        }

        let mut builder = TxBuilder::new();

        for note in &recovery.notes {
            builder.add_input(Input {
                input: wire::Input::Mint(MintInput { note: note.note() }),
                keypair: note.keypair,
                amount: note.amount(),
                fee: self.cfg.input_fee,
            });
        }

        self.finalize_and_submit_tx(dbtx, operation, builder, |txid| events::RecoveryEvent {
            amount,
            txid: Some(txid),
        })
        .expect("Recovery sweep must fund from the recovered notes themselves");

        operation
    }

    /// Hand out the next counter for `denomination` and persist the bump in
    /// the caller's dbtx, so a counter is only spent once the transaction
    /// carrying its blinded message is committed to.
    fn next_counter(&self, dbtx: &WriteTx, denomination: Denomination) -> u64 {
        let counter = dbtx
            .get(&CounterTable(self.federation), &denomination)
            .unwrap_or(0);

        dbtx.insert(
            &CounterTable(self.federation),
            &denomination,
            &(counter + 1),
        );

        counter
    }
}

impl MintClientModule {
    pub fn new(
        federation: FederationId,
        cfg: MintConfigConsensus,
        context: ClientContext,
        secret: MintSecret,
        tg: &TaskGroup,
    ) -> anyhow::Result<MintClientModule> {
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

        Ok(MintClientModule {
            federation,
            cfg,
            secret,
            client_ctx: context,
            tx_submission_executor,
            mint_executor,
            send_executor,
        })
    }
}

#[derive(Clone)]
pub struct MintClientModule {
    federation: FederationId,
    cfg: MintConfigConsensus,
    secret: MintSecret,
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
        operation: OperationId,
        mut builder: TxBuilder,
        event: impl FnOnce(TransactionId) -> E,
    ) -> Option<TransactionId> {
        let deficit = builder.deficit();

        let (spendable_notes, issuance_requests) = self.balance(dbtx, &mut builder)?;

        let funding: Amount = spendable_notes.iter().map(|n| n.amount()).sum();

        let remint = funding.saturating_sub(deficit);

        let txid = self.submit(dbtx, operation, builder, remint, event);

        if !spendable_notes.is_empty() || !issuance_requests.is_empty() {
            let sm = MintStateMachine {
                operation,
                spendable_notes,
                txid,
                issuance_requests,
            };
            self.mint_executor.add_state_machine_dbtx(dbtx, sm);
        }

        Some(txid)
    }

    /// Mint-side transaction balancing. Pulls funding notes from the wallet
    /// when the builder is underfunded, then absorbs any excess as change
    /// outputs. Sub-denomination dust below `smallest_denom + output_fee` is
    /// left as implicit federation revenue. Returns `None` iff the wallet
    /// holds insufficient funds to cover the builder's deficit — the sole
    /// failure mode after tx-too-large became a programmer-error panic in
    /// [`Mint::submit`].
    fn balance(
        &self,
        dbtx: &WriteTx,
        builder: &mut TxBuilder,
    ) -> Option<(Vec<SpendableNote>, Vec<NoteIssuanceRequest>)> {
        let mut spendable_notes = self.select_funding_input(dbtx, builder.deficit())?;

        // Sort by denomination to minimize information leaked about
        // which notes the wallet held.
        spendable_notes.sort_by_key(|note| note.denomination);

        for note in &spendable_notes {
            Self::remove_spendable_note(dbtx, self.federation, note);
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
            let counter = self.next_counter(dbtx, d);

            issuance_requests.push(NoteIssuanceRequest::new(d, counter, &self.secret));
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

    /// Sign the builder, size-check the encoded transaction, spawn the
    /// `TxSubmissionStateMachine`, log the caller's `event` followed by
    /// `TxCreateEvent`.
    fn submit<E: picomint_eventlog::Event + Send>(
        &self,
        dbtx: &WriteTx,
        operation: OperationId,
        builder: TxBuilder,
        remint: Amount,
        event: impl FnOnce(TransactionId) -> E,
    ) -> TransactionId {
        let fee = builder.total_fee();
        let tx = builder.build();

        assert!(
            tx.consensus_encode_to_vec().len() <= Transaction::MAX_TX_SIZE,
            "The generated transaction is too large.",
        );

        let txid = tx.compute_txid();

        let sm = TxSubmissionStateMachine { operation, tx };

        self.tx_submission_executor.add_state_machine_dbtx(dbtx, sm);

        self.client_ctx.log_event(dbtx, operation, event(txid));

        self.client_ctx
            .log_event(dbtx, operation, crate::TxCreateEvent { txid, remint, fee });

        txid
    }

    pub fn get_balance(&self, dbtx: &impl picomint_redb::DbRead) -> Amount {
        Self::get_count_by_denomination_dbtx(dbtx, self.federation)
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
        excess_output: Amount,
    ) -> Option<Vec<SpendableNote>> {
        let mut selected = Vec::new();
        let mut target_notes = Vec::new();

        let all_notes: Vec<SpendableNote> = dbtx.iter(&NoteTable(self.federation), |r| {
            r.map(|(note, ())| note).collect()
        });

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
    /// Count the `ECash` notes in the client's database by denomination.
    pub fn get_count_by_denomination(&self) -> BTreeMap<Denomination, u64> {
        let dbtx = self.client_ctx.db().begin_write();

        Self::get_count_by_denomination_dbtx(&dbtx, self.federation)
    }

    fn get_count_by_denomination_dbtx(
        dbtx: &impl picomint_redb::DbRead,
        federation: FederationId,
    ) -> BTreeMap<Denomination, u64> {
        dbtx.iter(&NoteTable(federation), |r| {
            let mut acc = BTreeMap::new();
            for (note, ()) in r {
                acc.entry(note.denomination)
                    .and_modify(|count| *count += 1)
                    .or_insert(1);
            }
            acc
        })
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
    pub async fn send(&self, amount: Amount) -> Result<ECash, SendECashError> {
        let amount = round_to_multiple(amount, client_denominations().next().unwrap().amount());

        let operation = OperationId::new_random();

        // Fast path: the wallet already has notes that sum exactly to
        // `amount`. Pull them out and emit `SendEvent` + `SendSuccessEvent`
        // atomically in one dbtx — no tx, no SM.
        let dbtx = self.client_ctx.db().begin_write();

        if let Some(ecash) = send_ecash_dbtx(&dbtx, self.federation, amount) {
            self.client_ctx
                .log_event(&dbtx, operation, SendEvent { amount });
            self.client_ctx.log_event(
                &dbtx,
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
            let counter = self.next_counter(&dbtx, d);

            issuance_requests.push(NoteIssuanceRequest::new(d, counter, &self.secret));
        }

        let mut builder = TxBuilder::new();
        for request in &issuance_requests {
            builder.add_output(Output {
                output: wire::Output::Mint(request.output()),
                amount: request.denomination.amount(),
                fee: self.cfg.output_fee,
            });
        }

        let deficit = builder.deficit();

        let (funding_notes, change_requests) = self
            .balance(&dbtx, &mut builder)
            .ok_or(SendECashError::InsufficientBalance)?;

        let funding: Amount = funding_notes.iter().map(|n| n.amount()).sum();

        let remint = funding.saturating_sub(deficit);

        let fee = builder.total_fee();
        let tx = builder.build();

        if tx.consensus_encode_to_vec().len() > Transaction::MAX_TX_SIZE {
            return Err(SendECashError::Failure);
        }

        let txid = tx.compute_txid();

        // Everything past this point lands in the same dbtx that submits
        // the reissuance: SendEvent → RemintEvent → TxCreateEvent →
        // MintSM + SendSM. A crash before the commit leaves no half-state
        // behind; on restart the operation simply doesn't exist.
        self.tx_submission_executor
            .add_state_machine_dbtx(&dbtx, TxSubmissionStateMachine { operation, tx });

        self.client_ctx
            .log_event(&dbtx, operation, SendEvent { amount });

        self.client_ctx
            .log_event(&dbtx, operation, RemintEvent { txid });

        self.client_ctx
            .log_event(&dbtx, operation, crate::TxCreateEvent { txid, remint, fee });

        issuance_requests.extend(change_requests);

        let mint_sm = MintStateMachine {
            operation,
            spendable_notes: funding_notes,
            txid,
            issuance_requests,
        };

        self.mint_executor.add_state_machine_dbtx(&dbtx, mint_sm);

        let send_sm = SendStateMachine { operation, amount };

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

    /// Receive the `ECash` by reissuing the notes. This method is idempotent
    /// via the deterministic [`OperationId`] derived from the ecash bytes.
    pub fn receive(&self, ecash: &ECash) -> Result<OperationId, ReceiveECashError> {
        let operation = OperationId::from_encodable(ecash);

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
            return Ok(operation);
        }

        let amount = ecash.amount();

        self.finalize_and_submit_tx(&dbtx, operation, tx_builder, |txid| ReceiveEvent {
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
        spendable_note: &SpendableNote,
    ) {
        dbtx.remove(&NoteTable(federation), spendable_note)
            .expect("Must delete existing spendable note");
    }
}

/// Pull a set of `SpendableNote`s out of `NoteTable` whose denominations sum
/// exactly to `remaining_amount`, remove them, and return the resulting
/// `ECash`. Returns `None` if no exact-match combination exists. No
/// events are logged — callers do that.
fn send_ecash_dbtx(
    dbtx: &WriteTx,
    federation: FederationId,
    mut remaining_amount: Amount,
) -> Option<ECash> {
    let mut sorted: Vec<SpendableNote> = dbtx.iter(&NoteTable(federation), |r| {
        r.map(|(note, ())| note).collect()
    });

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
        dbtx.remove(&NoteTable(federation), spendable_note)
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
    #[error("The ECash is from a different federation")]
    WrongFederation,
    #[error("ECash contains an uneconomical denomination")]
    UneconomicalDenomination,
    #[error("Receiving ecash requires additional funds")]
    InsufficientFunds,
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
