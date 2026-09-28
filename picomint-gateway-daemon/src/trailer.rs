//! Daemon-wide trailer task.
//!
//! The `ReceiveStateMachine` in `picomint-client::gateway` is purely mint-
//! local — it submits the incoming-contract tx and writes the terminal
//! `ReceiveSuccess` / `ReceiveFailure` event. The trailer watches the
//! global event log and, for a direct swap (the payment hash is
//! `Payment::Swapped`), settles the send the swap pays so the sender gets
//! the preimage (or refund signature). An inbound HTLC needs nothing here:
//! it was settled when its funding was submitted, since the gateway holds
//! the preimage.
//!
//! Cursor is persisted daemon-wide in `EventLogCursorTable` and advanced after
//! each dispatched event. Dispatches are idempotent, so on a crash the
//! trailer just re-runs the last event on restart.
use picomint_client::eventlog::EventLogEntry;
use picomint_client::gateway::events::{ReceiveFailureEvent, ReceiveSuccessEvent};
use picomint_core::Amount;
use picomint_redb::{DbRead, WriteTx};
use tracing::error;

use crate::AppState;
use crate::db::{EventLogCursorTable, Payment, PaymentTable};

const CHUNK_SIZE: u64 = 1_000;

pub async fn run(state: AppState) {
    let mut cursor = state
        .gateway_db
        .begin_read()
        .get(&EventLogCursorTable, &())
        .unwrap_or_default();

    let notify = state.client.event_notify();

    loop {
        let notified = notify.notified();

        let chunk = state.client.get_event_log(cursor, CHUNK_SIZE);

        for (id, entry) in &chunk {
            let dbtx = state.gateway_db.begin_write();

            dispatch(&state, &dbtx, entry);

            cursor = id.saturating_add(1);

            dbtx.insert(&EventLogCursorTable, &(), &cursor);

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
    } else if entry.to_event::<ReceiveFailureEvent>().is_some() {
        None
    } else {
        return;
    };

    // The daemon funds every incoming contract under the operation of its
    // payment hash.
    let payment_hash = entry.operation.0;

    match (tx_ref.get(&PaymentTable, &payment_hash), preimage) {
        (Some(Payment::Swapped { operation }), preimage) => state.settle_send(
            tx_ref,
            payment_hash,
            operation,
            // An internal settlement routes nothing, so a successful one
            // realized no routing cost.
            preimage.map(|preimage| (preimage, Amount::ZERO)),
        ),
        // An inbound HTLC was settled when its funding was submitted, so a
        // rejected funding leaves the recipient owed what the gateway was
        // paid; nothing here can make that good.
        (Some(Payment::Received), None) => {
            error!(operation = %entry.operation, "An inbound payment's funding was rejected after its HTLC settled");
        }
        _ => {}
    }
}
