//! Per-federation trailer task.
//!
//! The `ReceiveStateMachine` in `picomint-client::gw` is purely federation-
//! local — it submits the incoming-contract tx, gathers TPE shares, writes
//! the terminal `ReceiveSuccess` / `ReceiveRefund` / `ReceiveFailure` event,
//! and submits the refund tx for refunds. The trailer watches that event log
//! and drives the external side effect that makes the payment terminal from
//! the outside world's point of view:
//!
//! - Direct swap (daemon DB has an `OUTGOING_CONTRACT[op_id]` row): call
//!   `finalize_send` on the sending federation's client so the sender gets
//!   the preimage (or refund signature).
//! - External LN receive (no outgoing row): call `claim_for_hash` /
//!   `fail_for_hash` on the LDK node so the upstream LN sender's HTLC
//!   settles or times out.
//!
//! Cursor is persisted per federation in the daemon DB (`EVENT_CURSOR`)
//! and advanced after each dispatched event. Dispatches are idempotent, so
//! on a crash the trailer just re-runs the last event on restart.
use std::sync::Arc;

use bitcoin::hashes::{Hash as _, sha256};
use lightning::types::payment::{PaymentHash, PaymentPreimage};
use picomint_client::Client;
use picomint_client::gw::events::{ReceiveRefundEvent, ReceiveSuccessEvent};
use picomint_core::config::FederationId;
use picomint_core::core::OperationId;
use picomint_core::ln::contracts::PaymentImage;
use picomint_core::task::TaskGroup;
use picomint_eventlog::EventLogEntry;
use picomint_redb::WriteTxRef;

use crate::AppState;
use crate::db::{EVENT_CURSOR, INCOMING_CONTRACT, OUTGOING_CONTRACT};

const CHUNK_SIZE: u64 = 1_000;

pub fn spawn_trailer(
    task_group: &TaskGroup,
    state: AppState,
    federation_id: FederationId,
    client: Arc<Client>,
) {
    task_group.spawn_cancellable("gw-trailer", run(state, federation_id, client));
}

async fn run(state: AppState, federation_id: FederationId, client: Arc<Client>) {
    let mut cursor = state
        .gateway_db
        .begin_read()
        .as_ref()
        .get(&EVENT_CURSOR, &federation_id)
        .unwrap_or_default();

    let notify = client.event_notify();

    loop {
        let notified = notify.notified();

        let chunk = client.get_event_log(Some(cursor), CHUNK_SIZE).await;

        for (id, entry) in &chunk {
            let dbtx = state.gateway_db.begin_write();

            dispatch(&state, &dbtx.as_ref(), entry);

            cursor = id.saturating_add(1);

            dbtx.insert(&EVENT_CURSOR, &federation_id, &cursor);

            dbtx.commit();
        }

        if (chunk.len() as u64) < CHUNK_SIZE {
            notified.await;
        }
    }
}

fn dispatch(state: &AppState, tx_ref: &WriteTxRef<'_>, entry: &EventLogEntry) {
    let preimage = if let Some(ev) = entry.to_event::<ReceiveSuccessEvent>() {
        Some(ev.preimage)
    } else if entry.to_event::<ReceiveRefundEvent>().is_some() {
        None
    } else {
        return;
    };

    let op_id = entry.operation_id;

    if let Some(row) = tx_ref.get(&OUTGOING_CONTRACT, &op_id) {
        dispatch_direct_swap(state, tx_ref, op_id, row, preimage);
    } else {
        dispatch_ln_receive(state, tx_ref, op_id, preimage);
    }
}

fn dispatch_direct_swap(
    state: &AppState,
    tx_ref: &WriteTxRef<'_>,
    op_id: OperationId,
    row: crate::db::OutgoingContractRow,
    preimage: Option<[u8; 32]>,
) {
    let source_client = state
        .select_client(row.federation_id)
        .expect("source federation for outgoing contract is connected");

    source_client.gw().finalize_send(
        &tx_ref.isolate(row.federation_id),
        op_id,
        row.contract,
        row.outpoint,
        preimage,
        // Direct swap — no LN hop, no routing cost to record.
        picomint_core::Amount::ZERO,
    );
}

fn dispatch_ln_receive(
    state: &AppState,
    tx_ref: &WriteTxRef<'_>,
    op_id: OperationId,
    preimage: Option<[u8; 32]>,
) {
    // Refund path: the federation-side refund tx already reclaims the
    // contract amount for us. We intentionally do NOT fail the inbound LDK
    // HTLC — let it expire on LDK's own schedule.
    let Some(preimage) = preimage else {
        return;
    };

    let row = tx_ref
        .get(&INCOMING_CONTRACT, &op_id)
        .expect("incoming_contract row registered by create_bolt11_invoice");

    let ph = match row.contract.commitment.payment_image {
        PaymentImage::Hash(h) => PaymentHash(*sha256::Hash::as_byte_array(&h)),
        PaymentImage::Point(_) => {
            unreachable!("create_bolt11_invoice rejects non-Hash payment images")
        }
    };

    state
        .node
        .bolt11_payment()
        .claim_for_hash(ph, row.amount.msats, PaymentPreimage(preimage))
        .expect("LDK has this payment_hash (registered via receive_for_hash)");
}
