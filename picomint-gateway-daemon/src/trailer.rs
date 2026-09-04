//! Daemon-wide trailer task.
//!
//! The `ReceiveStateMachine` in `picomint-client::gateway` is purely mint-
//! local — it submits the incoming-contract tx, gathers TPE shares, writes
//! the terminal `ReceiveSuccess` / `ReceiveRefund` / `ReceiveFailure` event,
//! and submits the refund tx for refunds. The trailer watches the global
//! event log and drives the external side effect that makes the payment
//! terminal from the outside world's point of view:
//!
//! - Direct swap (daemon DB has an `OutgoingContract[operation]` row): call
//!   `gateway_finalize_send` on the sending mint's client so the sender
//!   gets the preimage (or refund signature).
//! - External LN receive (no outgoing row): call `claim_for_hash` on the
//!   LDK node so the upstream LN sender's HTLC settles; on refund the
//!   inbound HTLC is left to expire on LDK's schedule.
//!
//! Cursor is persisted daemon-wide in `EventCursorTable` and advanced after
//! each dispatched event. Dispatches are idempotent, so on a crash the
//! trailer just re-runs the last event on restart.
use bitcoin::hashes::Hash as _;
use lightning::types::payment::{PaymentHash, PaymentPreimage};
use picomint_client::eventlog::EventLogEntry;
use picomint_client::gateway::events::{ReceiveRefundEvent, ReceiveSuccessEvent};
use picomint_core::core::OperationId;
use picomint_redb::{DbRead, WriteTx};
use tracing::error;

use crate::AppState;
use crate::db::{EventCursorTable, IncomingOfferTable, OutgoingContractTable};

const CHUNK_SIZE: u64 = 1_000;

pub async fn run(state: AppState) {
    let mut cursor = state
        .gateway_db
        .begin_read()
        .get(&EventCursorTable, &())
        .unwrap_or_default();

    let notify = state.client.event_notify();

    loop {
        let notified = notify.notified();

        let chunk = state.client.get_event_log(cursor, CHUNK_SIZE);

        for (id, entry) in &chunk {
            let dbtx = state.gateway_db.begin_write();

            dispatch(&state, &dbtx, entry);

            cursor = id.saturating_add(1);

            dbtx.insert(&EventCursorTable, &(), &cursor);

            dbtx.commit();
        }

        if (chunk.len() as u64) < CHUNK_SIZE {
            notified.await;
        }
    }
}

fn dispatch(state: &AppState, tx_ref: &WriteTx, entry: &EventLogEntry) {
    let preimage = if let Some(ev) = entry.to_event::<ReceiveSuccessEvent>() {
        Some(ev.preimage)
    } else if entry.to_event::<ReceiveRefundEvent>().is_some() {
        None
    } else {
        return;
    };

    let operation = entry.operation;

    if let Some(row) = tx_ref.get(&OutgoingContractTable, &operation) {
        dispatch_direct_swap(state, tx_ref, operation, row, preimage);
    } else {
        dispatch_lightning_receive(state, tx_ref, operation, preimage);
    }
}

fn dispatch_direct_swap(
    state: &AppState,
    tx_ref: &WriteTx,
    operation: OperationId,
    row: crate::db::OutgoingContractRow,
    preimage: Option<[u8; 32]>,
) {
    state
        .client
        .gateway_finalize_send(
            row.mint,
            tx_ref,
            operation,
            row.contract,
            row.outpoint,
            // An internal settlement routes nothing, so a successful one
            // realized no routing cost.
            preimage.map(|preimage| (preimage, picomint_core::Amount::ZERO)),
        )
        .expect("source mint for outgoing contract is added");
}

fn dispatch_lightning_receive(
    state: &AppState,
    tx_ref: &WriteTx,
    operation: OperationId,
    preimage: Option<[u8; 32]>,
) {
    // Refund path: the mint-side refund tx already reclaims the
    // contract amount for us. We intentionally do NOT fail the inbound LDK
    // HTLC — let it expire on LDK's own schedule.
    let Some(preimage) = preimage else {
        return;
    };

    // Removing the offer's mint wipes the row; an event of its that
    // the cursor had not yet passed then has nothing left to claim, and the
    // inbound HTLC expires on LDK's own schedule.
    let Some(row) = tx_ref.get(&IncomingOfferTable, &operation) else {
        error!("Cannot claim HTLC for a removed mint");

        return;
    };

    let ph = PaymentHash(*row.offer.commitment.payment_hash.as_byte_array());

    state
        .node
        .bolt11_payment()
        .claim_for_hash(
            ph,
            row.offer.commitment.amount.msat,
            PaymentPreimage(preimage),
        )
        .expect("LDK has this payment_hash (registered via receive_for_hash)");
}
