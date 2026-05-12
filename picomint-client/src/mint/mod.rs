pub use picomint_core::mint as common;

mod api;
mod client_db;
mod ecash;
mod events;
mod issuance;
mod mint_sm;
mod secret;
mod send_sm;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use crate::api::FederationApi;
use crate::executor::ModuleExecutor;
use crate::module::ClientContext;
use crate::task::TaskGroup;
use crate::tx::{Input, Output, TxBuilder};
use crate::tx::{Transaction, TxSubmissionSmContext, TxSubmissionStateMachine};
use anyhow::{Context as _, bail};
use bitcoin_hashes::sha256;
use client_db::{NOTE, RECEIVE_OPERATION, RECOVERY, Recovery};
pub use events::*;
use futures::StreamExt;
use picomint_core::config::FederationId;
use picomint_core::core::OperationId;
use picomint_core::mint::config::{MintConfigConsensus, client_denominations};
use picomint_core::mint::{Denomination, MintInput, Note, RecoveryItem};
use picomint_core::secp256k1::rand::{Rng, thread_rng};
use picomint_core::secp256k1::{Keypair, XOnlyPublicKey};
use picomint_core::{Amount, PeerId, TransactionId, wire};
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::WriteTxRef;
use rand::seq::IteratorRandom;
use tbs::{AggregatePublicKey, aggregate_signature_shares};
use thiserror::Error;

pub use self::ecash::ECash;
use self::issuance::NoteIssuanceRequest;
use self::mint_sm::MintStateMachine;
pub use self::secret::MintSecret;
use self::send_sm::SendStateMachine;

const TARGET_PER_DENOMINATION: usize = 3;
const SLICE_SIZE: u64 = 10000;
const PARALLEL_HASH_REQUESTS: usize = 10;
const PARALLEL_SLICE_REQUESTS: usize = 10;

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

/// Seed the mint recovery state. Caller writes this in the same tx that
/// persists their `CLIENT_CONFIG` row, so "join + start recovery" is one
/// atomic commit. The driver picks the row up the next time
/// [`MintClientModule::new`] runs and finally emits a single terminal
/// `RecoveryEvent` under the returned operation id (also persisted in
/// the row, so a restart's driver completes under the same op id).
///
/// `total_items` is left as `None` — the driver fills it in via
/// `module_api.recovery_count()` on its first awakening, so this entry
/// point doesn't have to hit the network.
///
/// Live progress is observable via
/// [`crate::Client::subscribe_recovery_progress`] (no events are
/// emitted on each batch).
///
/// Panics if a recovery is already in progress.
pub fn init_recovery(dbtx: &WriteTxRef<'_>) -> OperationId {
    let operation = OperationId::new_random();

    let state = Recovery {
        operation,
        next_index: 0,
        total_items: None,
        requests: BTreeMap::new(),
        nonces: BTreeSet::new(),
    };

    assert!(
        dbtx.insert(&RECOVERY, &(), &state).is_none(),
        "init_recovery called when a recovery is already in progress"
    );

    operation
}

impl MintClientModule {
    /// Drive recovery to completion: fill in `total_items` if missing,
    /// download slices, checkpoint on each batch, and on the final
    /// batch hand off to `finalize_recovery`, which submits a
    /// reissuance tx that re-mints the recovered notes under fresh
    /// blinded outputs. From `TxAcceptEvent` on, the op rides the
    /// standard mint state machines.
    async fn run_recovery(self, mut state: Recovery) -> anyhow::Result<()> {
        let module_api = self.client_ctx.api();
        let db = self.client_ctx.db().clone();

        // First awakening of a freshly-init'd recovery: fetch the count
        // from the federation and persist it. The RECOVERY-table commit
        // wakes any subscribers of `subscribe_recovery_progress`.
        let total_items = match state.total_items {
            Some(t) => t,
            None => {
                let total = module_api.recovery_count().await?;

                state.total_items = Some(total);

                let dbtx = db.begin_write();

                dbtx.insert(&RECOVERY, &(), &state);

                dbtx.commit();

                total
            }
        };

        // Re-entry case: scan was already complete on disk, jump to
        // finalisation directly.
        if state.next_index == total_items {
            return self.finalize_recovery(state).await;
        }

        let peer_selector = PeerSelector::new(module_api.all_peers());

        let mut recovery_stream =
            futures::stream::iter((state.next_index..total_items).step_by(SLICE_SIZE as usize))
                .map(|start| {
                    let api = module_api.clone();
                    let end = std::cmp::min(start + SLICE_SIZE, total_items);

                    async move { (start, end, api.recovery_slice_hash(start, end).await) }
                })
                .buffered(PARALLEL_HASH_REQUESTS)
                .map(|(start, end, hash)| {
                    download_slice_with_hash(
                        module_api.clone(),
                        peer_selector.clone(),
                        start,
                        end,
                        hash,
                    )
                })
                .buffered(PARALLEL_SLICE_REQUESTS);

        let tweak_filter = self.secret.tweak_filter();

        loop {
            let items = recovery_stream
                .next()
                .await
                .context("Recovery stream finished before recovery is complete")?;

            for item in &items {
                match item {
                    RecoveryItem::Output {
                        denomination,
                        nonce_hash,
                        tweak,
                    } => {
                        if !issuance::check_tweak(tweak_filter, *tweak) {
                            continue;
                        }

                        let output_secret =
                            issuance::output_secret(*denomination, *tweak, &self.secret);

                        if !issuance::check_nonce(&output_secret, *nonce_hash) {
                            continue;
                        }

                        let computed_nonce_hash = issuance::nonce(&output_secret).consensus_hash();

                        // Ignore possible duplicate nonces
                        if !state.nonces.insert(computed_nonce_hash) {
                            continue;
                        }

                        state.requests.insert(
                            computed_nonce_hash,
                            NoteIssuanceRequest::new(*denomination, *tweak, &self.secret),
                        );
                    }
                    RecoveryItem::Input { nonce_hash } => {
                        state.requests.remove(nonce_hash);
                        state.nonces.remove(nonce_hash);
                    }
                }
            }

            state.next_index += items.len() as u64;

            // Final batch: skip the per-batch checkpoint and let
            // `finalize_recovery` commit the reissuance-tx submission
            // and the terminal `RecoveryEvent` in one atomic dbtx.
            if state.next_index == total_items {
                return self.finalize_recovery(state).await;
            }

            let dbtx = db.begin_write();

            dbtx.insert(&RECOVERY, &(), &state);

            dbtx.commit();
        }
    }

    /// Final phase of recovery: fetch shares for the recovered nonces,
    /// materialise `SpendableNote`s, and submit a single reissuance tx
    /// — atomically with deletion of the `RECOVERY` row and emission
    /// of the terminal `RecoveryEvent`.
    async fn finalize_recovery(self, state: Recovery) -> anyhow::Result<()> {
        let module_api = self.client_ctx.api();
        let db = self.client_ctx.db().clone();
        let federation_id = self.client_ctx.federation_id();

        let issuance_requests: Vec<NoteIssuanceRequest> = state.requests.into_values().collect();

        let mut spendable_notes = Vec::with_capacity(issuance_requests.len());

        if !issuance_requests.is_empty() {
            let shares = module_api
                .signature_shares_recovery(issuance_requests.clone(), self.cfg.tbs_pks.clone())
                .await;

            for (i, request) in issuance_requests.iter().enumerate() {
                let shares = shares
                    .iter()
                    .map(|(peer, peer_shares)| (peer.to_usize() as u64, peer_shares[i]))
                    .collect();

                let note = request.finalize(aggregate_signature_shares(&shares));

                spendable_notes.push(note);
            }
        }

        let amount: Amount = spendable_notes.iter().map(|n| n.amount()).sum();

        let dbtx = db.begin_write();

        let operation = state.operation;

        dbtx.remove(&RECOVERY, &());

        if !spendable_notes.is_empty() {
            let mut builder = TxBuilder::new();
            for note in &spendable_notes {
                builder.add_input(Input {
                    input: wire::Input::Mint(MintInput { note: note.note() }),
                    keypair: note.keypair,
                    amount: note.amount(),
                    fee: self.cfg.input_fee,
                });
            }

            self.finalize_and_submit_tx(&dbtx.as_ref(), operation, builder, |txid| {
                events::RecoveryEvent {
                    amount,
                    txid: Some(txid),
                }
            })?;
        } else {
            picomint_eventlog::log_event(
                &dbtx.as_ref(),
                federation_id,
                operation,
                events::RecoveryEvent { amount, txid: None },
            );
        }

        dbtx.commit();

        Ok(())
    }
}

impl MintClientModule {
    pub fn new(
        federation_id: FederationId,
        cfg: MintConfigConsensus,
        context: ClientContext,
        secret: MintSecret,
        tg: &TaskGroup,
    ) -> anyhow::Result<MintClientModule> {
        let (tweak_tx, tweak_rx) = async_channel::bounded(50);

        let filter = secret.tweak_filter();

        tokio::task::spawn_blocking(move || {
            loop {
                let tweak: [u8; 16] = thread_rng().r#gen();

                if !issuance::check_tweak(filter, tweak) {
                    continue;
                }

                if tweak_tx.send_blocking(tweak).is_err() {
                    return;
                }
            }
        });

        let sm_context = MintSmContext {
            client_ctx: context.clone(),
            federation_id,
            tbs_agg_pks: cfg.tbs_agg_pks.clone(),
            tbs_pks: cfg.tbs_pks.clone(),
        };

        let mint_executor =
            ModuleExecutor::new(context.db().clone(), sm_context.clone(), tg.clone());

        let send_executor = ModuleExecutor::new(context.db().clone(), sm_context, tg.clone());

        let tx_submission_executor = ModuleExecutor::new(
            context.db().clone(),
            TxSubmissionSmContext {
                api: context.api(),
                federation_id,
            },
            tg.clone(),
        );

        let module = MintClientModule {
            federation_id,
            cfg,
            secret,
            client_ctx: context,
            tweak_rx,
            tx_submission_executor,
            mint_executor,
            send_executor,
        };

        // If a recovery row was seeded (by `Client::init_recovery`) and
        // hasn't been cleaned up yet, drive it to completion in the
        // background. The driver wipes the row when done, so a clean
        // shutdown mid-recovery just resumes on the next boot.
        if let Some(state) = module.client_ctx.db().begin_read().get(&RECOVERY, &()) {
            let module = module.clone();
            tg.spawn(module.run_recovery(state));
        }

        Ok(module)
    }
}

#[derive(Clone)]
pub struct MintClientModule {
    federation_id: FederationId,
    cfg: MintConfigConsensus,
    secret: MintSecret,
    client_ctx: ClientContext,
    tweak_rx: async_channel::Receiver<[u8; 16]>,
    tx_submission_executor: ModuleExecutor<TxSubmissionStateMachine>,
    mint_executor: ModuleExecutor<MintStateMachine>,
    send_executor: ModuleExecutor<SendStateMachine>,
}

/// Context handed to per-SM executors. Keeps the `ClientContext` handle
/// plus the immutable config data SMs need.
#[derive(Clone)]
pub struct MintSmContext {
    pub client_ctx: ClientContext,
    pub federation_id: FederationId,
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
        dbtx: &WriteTxRef<'_>,
        operation: OperationId,
        mut builder: TxBuilder,
        event: impl FnOnce(TransactionId) -> E,
    ) -> anyhow::Result<TransactionId> {
        let deficit = builder.deficit();

        let (spendable_notes, issuance_requests) = self.balance(dbtx, &mut builder)?;

        let funding: Amount = spendable_notes.iter().map(|n| n.amount()).sum();

        let remint = funding.saturating_sub(deficit);

        let txid = self.submit(dbtx, operation, builder, remint, event)?;

        if !spendable_notes.is_empty() || !issuance_requests.is_empty() {
            let sm = MintStateMachine {
                operation,
                spendable_notes,
                txid,
                issuance_requests,
            };
            self.mint_executor.add_state_machine_dbtx(dbtx, sm);
        }

        Ok(txid)
    }

    /// Mint-side transaction balancing. Pulls funding notes from the wallet
    /// when the builder is underfunded, then absorbs any excess as change
    /// outputs. Sub-denomination dust below `smallest_denom + output_fee` is
    /// left as implicit federation revenue.
    fn balance(
        &self,
        dbtx: &WriteTxRef<'_>,
        builder: &mut TxBuilder,
    ) -> anyhow::Result<(Vec<SpendableNote>, Vec<NoteIssuanceRequest>)> {
        let mut spendable_notes = self
            .select_funding_input(dbtx, builder.deficit())
            .context("Insufficient funds")?;

        // Sort by denomination to minimize information leaked about
        // which notes the wallet held.
        spendable_notes.sort_by_key(|note| note.denomination);

        for note in &spendable_notes {
            Self::remove_spendable_note(dbtx, note);
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
            let tweak = self
                .tweak_rx
                .recv_blocking()
                .expect("Tweak generator task dropped its sender");

            issuance_requests.push(NoteIssuanceRequest::new(d, tweak, &self.secret));
        }

        for request in &issuance_requests {
            builder.add_output(Output {
                output: wire::Output::Mint(request.output()),
                amount: request.denomination.amount(),
                fee: self.cfg.output_fee,
            });
        }

        assert_eq!(builder.deficit(), Amount::ZERO);

        Ok((spendable_notes, issuance_requests))
    }

    /// Sign the builder, size-check the encoded transaction, spawn the
    /// `TxSubmissionStateMachine`, log the caller's `event` followed by
    /// `TxCreateEvent`.
    fn submit<E: picomint_eventlog::Event + Send>(
        &self,
        dbtx: &WriteTxRef<'_>,
        operation: OperationId,
        builder: TxBuilder,
        remint: Amount,
        event: impl FnOnce(TransactionId) -> E,
    ) -> anyhow::Result<TransactionId> {
        let fee = builder.total_fee();
        let tx = builder.build();

        if tx.consensus_encode_to_vec().len() > Transaction::MAX_TX_SIZE {
            bail!("The generated transaction is too large.");
        }

        let txid = tx.compute_txid();

        let sm = TxSubmissionStateMachine { operation, tx };

        self.tx_submission_executor.add_state_machine_dbtx(dbtx, sm);

        self.client_ctx.log_event(dbtx, operation, event(txid));

        self.client_ctx
            .log_event(dbtx, operation, crate::TxCreateEvent { txid, remint, fee });

        Ok(txid)
    }

    pub fn get_balance(&self, dbtx: &impl picomint_redb::DbRead) -> Amount {
        Self::get_count_by_denomination_dbtx(dbtx)
            .into_iter()
            .map(|(denomination, count)| denomination.amount().mul_u64(count))
            .sum()
    }

    pub fn balance_notify(&self) -> Arc<tokio::sync::Notify> {
        self.client_ctx.db().notify_for_table(&NOTE)
    }

    /// Yields the in-flight recovery's progress as a percentage
    /// (0.0..=100.0) on every commit touching the `RECOVERY` row.
    /// Returns immediately if no recovery is active at subscribe time;
    /// ends when `finalize_recovery` removes the row. Mirrors the shape
    /// of [`crate::Client::subscribe_balance_changes`] — UIs typically
    /// pair the live percentage with the terminal `RecoveryEvent` on
    /// the same operation id.
    pub fn subscribe_recovery_progress(&self) -> futures::stream::BoxStream<'static, f64> {
        let notify = self.client_ctx.db().notify_for_table(&RECOVERY);
        let db = self.client_ctx.db().clone();

        Box::pin(async_stream::stream! {
            loop {
                let notified = notify.notified();
                match db.begin_read().as_ref().get(&RECOVERY, &()) {
                    Some(state) => {
                        let percent = state
                            .total_items
                            .map(|total| (state.next_index as f64 / total as f64) * 100.0)
                            .unwrap_or(0.0);
                        yield percent;
                    }
                    None => return,
                }
                notified.await;
            }
        })
    }

    fn select_funding_input(
        &self,
        dbtx: &WriteTxRef<'_>,
        mut excess_output: Amount,
    ) -> Option<Vec<SpendableNote>> {
        let mut selected_notes = Vec::new();
        let mut target_notes = Vec::new();

        let all_notes: Vec<SpendableNote> =
            dbtx.iter(&NOTE, |r| r.map(|(note, ())| note).collect());

        for amount in client_denominations().rev() {
            let notes_amount: Vec<SpendableNote> = all_notes
                .iter()
                .filter(|note| note.denomination == amount)
                .cloned()
                .collect();

            target_notes.extend(notes_amount.iter().take(TARGET_PER_DENOMINATION).cloned());

            for note in notes_amount.into_iter().skip(TARGET_PER_DENOMINATION) {
                let note_value = note
                    .amount()
                    .checked_sub(self.cfg.input_fee)
                    .expect("All our notes are economical");

                excess_output = excess_output.saturating_sub(note_value);

                selected_notes.push(note);
            }
        }

        if excess_output == Amount::ZERO {
            return Some(selected_notes);
        }

        for note in target_notes {
            let note_value = note
                .amount()
                .checked_sub(self.cfg.input_fee)
                .expect("All our notes are economical");

            excess_output = excess_output.saturating_sub(note_value);

            selected_notes.push(note);

            if excess_output == Amount::ZERO {
                return Some(selected_notes);
            }
        }

        None
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

        Self::get_count_by_denomination_dbtx(&dbtx.as_ref())
    }

    fn get_count_by_denomination_dbtx(
        dbtx: &impl picomint_redb::DbRead,
    ) -> BTreeMap<Denomination, u64> {
        dbtx.iter(&NOTE, |r| {
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
    /// amount will be rounded up to a multiple of 512 msats which is the
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

        if let Some(ecash) = send_ecash_dbtx(&dbtx.as_ref(), self.federation_id, amount) {
            self.client_ctx
                .log_event(&dbtx.as_ref(), operation, SendEvent { amount });
            self.client_ctx.log_event(
                &dbtx.as_ref(),
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

        let target_denominations = represent_amount(amount);

        // Build target issuance requests up-front. Their outputs go into the
        // builder first; the balance loop then pulls funding from the wallet
        // and appends change outputs. We extend `issuance_requests` with the
        // change requests after balance so the order matches the transaction's
        // outputs and a single `MintStateMachine` can process both.
        let mut issuance_requests: Vec<NoteIssuanceRequest> = Vec::new();
        for d in target_denominations {
            let tweak = self
                .tweak_rx
                .recv_blocking()
                .expect("Tweak generator task dropped its sender");
            issuance_requests.push(NoteIssuanceRequest::new(d, tweak, &self.secret));
        }

        let mut builder = TxBuilder::new();
        for request in &issuance_requests {
            builder.add_output(Output {
                output: wire::Output::Mint(request.output()),
                amount: request.denomination.amount(),
                fee: self.cfg.output_fee,
            });
        }

        let dbtx = self.client_ctx.db().begin_write();

        let deficit = builder.deficit();

        let (funding_notes, change_requests) = self
            .balance(&dbtx.as_ref(), &mut builder)
            .map_err(|_| SendECashError::InsufficientBalance)?;

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
            .add_state_machine_dbtx(&dbtx.as_ref(), TxSubmissionStateMachine { operation, tx });

        self.client_ctx
            .log_event(&dbtx.as_ref(), operation, SendEvent { amount });

        self.client_ctx
            .log_event(&dbtx.as_ref(), operation, RemintEvent { txid });

        self.client_ctx.log_event(
            &dbtx.as_ref(),
            operation,
            crate::TxCreateEvent { txid, remint, fee },
        );

        issuance_requests.extend(change_requests);

        let mint_sm = MintStateMachine {
            operation,
            spendable_notes: funding_notes,
            txid,
            issuance_requests,
        };

        self.mint_executor
            .add_state_machine_dbtx(&dbtx.as_ref(), mint_sm);

        let send_sm = SendStateMachine { operation, amount };

        self.send_executor
            .add_state_machine_dbtx(&dbtx.as_ref(), send_sm);

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

        if ecash.mint != self.federation_id {
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
            .as_ref()
            .insert(&RECEIVE_OPERATION, &operation, &())
            .is_some()
        {
            return Ok(operation);
        }

        let amount = ecash.amount();

        self.finalize_and_submit_tx(&dbtx.as_ref(), operation, tx_builder, |txid| ReceiveEvent {
            txid,
            amount,
        })
        .map_err(|_| ReceiveECashError::InsufficientFunds)?;

        dbtx.commit();

        Ok(operation)
    }

    fn remove_spendable_note(dbtx: &WriteTxRef<'_>, spendable_note: &SpendableNote) {
        dbtx.remove(&NOTE, spendable_note)
            .expect("Must delete existing spendable note");
    }
}

/// Pull a set of `SpendableNote`s out of `NOTE` whose denominations sum
/// exactly to `remaining_amount`, remove them, and return the resulting
/// `ECash`. Returns `None` if no exact-match combination exists. No
/// events are logged — callers do that.
fn send_ecash_dbtx(
    dbtx: &WriteTxRef<'_>,
    federation_id: FederationId,
    mut remaining_amount: Amount,
) -> Option<ECash> {
    let mut sorted: Vec<SpendableNote> = dbtx.iter(&NOTE, |r| r.map(|(note, ())| note).collect());

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
        dbtx.remove(&NOTE, spendable_note)
            .expect("Must delete existing spendable note");
    }

    Some(ECash::new(federation_id, notes))
}

/// Drop every redb table this module owns under the caller's prefix.
/// Called by [`crate::Client::wipe`] for end-of-life client cleanup.
pub(crate) fn wipe_tables(dbtx: &WriteTxRef<'_>) {
    dbtx.delete_table(&NOTE);
    dbtx.delete_table(&RECEIVE_OPERATION);
    dbtx.delete_table(&RECOVERY);
    dbtx.delete_table(&crate::executor::table::<MintStateMachine>());
    dbtx.delete_table(&crate::executor::table::<SendStateMachine>());
}

#[derive(Clone)]
struct PeerSelector {
    latency: Arc<RwLock<BTreeMap<PeerId, Duration>>>,
}

impl PeerSelector {
    fn new(peers: BTreeSet<PeerId>) -> Self {
        let latency = peers
            .into_iter()
            .map(|peer| (peer, Duration::ZERO))
            .collect();

        Self {
            latency: Arc::new(RwLock::new(latency)),
        }
    }

    /// Pick 2 peers at random, return the one with lower latency
    fn choose_peer(&self) -> PeerId {
        let latency = self.latency.read().unwrap();

        let peer_a = latency.iter().choose(&mut thread_rng()).unwrap();
        let peer_b = latency.iter().choose(&mut thread_rng()).unwrap();

        if peer_a.1 <= peer_b.1 {
            *peer_a.0
        } else {
            *peer_b.0
        }
    }

    // Update with exponential moving average (α = 0.1)
    fn report(&self, peer: PeerId, duration: Duration) {
        self.latency
            .write()
            .unwrap()
            .entry(peer)
            .and_modify(|latency| *latency = *latency * 9 / 10 + duration * 1 / 10)
            .or_insert(duration);
    }

    fn remove(&self, peer: PeerId) {
        self.latency.write().unwrap().remove(&peer);
    }
}

/// Download a slice with hash verification and peer selection
async fn download_slice_with_hash(
    module_api: FederationApi,
    peer_selector: PeerSelector,
    start: u64,
    end: u64,
    expected_hash: sha256::Hash,
) -> Vec<RecoveryItem> {
    const TIMEOUT: Duration = Duration::from_secs(30);

    loop {
        let peer = peer_selector.choose_peer();
        let start_time = SystemTime::now();

        if let Ok(data) = module_api.recovery_slice(peer, TIMEOUT, start, end).await {
            let elapsed = SystemTime::now()
                .duration_since(start_time)
                .unwrap_or_default();

            peer_selector.report(peer, elapsed);

            if data.consensus_hash::<sha256::Hash>() == expected_hash {
                return data;
            }

            peer_selector.remove(peer);
        } else {
            peer_selector.report(peer, TIMEOUT);
        }
    }
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
    Amount::from_msats(amount.msats.next_multiple_of(min_denomiation.msats))
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
