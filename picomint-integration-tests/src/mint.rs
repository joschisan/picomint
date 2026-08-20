use std::pin::pin;
use std::sync::Arc;

use anyhow::ensure;
use async_stream::stream;
use futures::StreamExt;
use picomint_client::mint::{MintSuccessEvent, ReceiveEvent, SendEvent};
use picomint_client::{Client, Mnemonic, TxAcceptEvent, TxRejectEvent};
use picomint_core::Amount;
use picomint_core::core::OperationId;
use picomint_eventlog::{EventLogEntry, EventLogId};
use tracing::info;

use crate::env::TestEnv;

#[derive(Debug)]
#[allow(dead_code)]
enum MintEvent {
    Send(SendEvent),
    Receive(ReceiveEvent),
}

fn mint_event_stream(
    client: &Arc<Client>,
) -> impl futures::Stream<Item = (OperationId, MintEvent)> {
    let client = client.clone();
    let notify = client.event_notify();
    let mut next_id = EventLogId::LOG_START;

    stream! {
        loop {
            let notified = notify.notified();
            let events = client.get_event_log(next_id, 100).await;

            for (id, entry) in events {
                next_id = id.saturating_add(1);

                if let Some((op, event)) = try_parse_mint_event(&entry) {
                    yield (op, event);
                }
            }

            notified.await;
        }
    }
}

fn try_parse_mint_event(entry: &EventLogEntry) -> Option<(OperationId, MintEvent)> {
    let op = entry.operation;
    if let Some(e) = entry.to_event() {
        return Some((op, MintEvent::Send(e)));
    }
    if let Some(e) = entry.to_event() {
        return Some((op, MintEvent::Receive(e)));
    }
    None
}

/// Wait until a receive operation is fully settled. Returns:
/// - `Ok` once both `TxAcceptEvent` AND `MintSuccessEvent` have been
///   observed — at that point the spendable notes have been written
///   to the local NoteTable table and the balance reflects the receive.
/// - `Err` on `TxRejectEvent` (federation rejected the tx).
///
/// Callers must wait for `MintSuccessEvent`, not just `TxAcceptEvent`,
/// because the issuance state machine still has to fetch threshold
/// signatures after the tx is accepted before the notes land. Reading
/// `get_balance()` between TxAccept and MintSuccessEvent returns a
/// stale (lower) figure.
async fn await_tx_outcome(client: &Arc<Client>, operation: OperationId) -> Result<(), String> {
    let mut stream = client.subscribe_operation_events(operation);

    let mut tx_accepted = false;

    while let Some(entry) = stream.next().await {
        if entry.to_event::<TxAcceptEvent>().is_some() {
            tx_accepted = true;
        }

        if let Some(ev) = entry.to_event::<TxRejectEvent>() {
            return Err(ev.error);
        }

        if tx_accepted && entry.to_event::<MintSuccessEvent>().is_some() {
            return Ok(());
        }
    }

    unreachable!("stream only ends at client shutdown")
}

pub async fn run_tests(env: &TestEnv, client_send: &Arc<Client>) -> anyhow::Result<()> {
    info!("mint: send_and_receive (10 iterations) + double_spend_is_rejected");

    // Capture the receive client's mnemonic so we can recover it at
    // the end of the suite, after it has accumulated a balance.
    let receive_mnemonic = Mnemonic::generate(12)?;
    let client_receive = env.new_client(Some(receive_mnemonic.clone())).await?;

    let mut send_events = pin!(mint_event_stream(client_send));
    let mut receive_events = pin!(mint_event_stream(&client_receive));

    for i in 0..10 {
        info!("Sending ecash payment {} of 10", i + 1);

        let ecash = client_send.mint().send(Amount::from_sat(1_000)).await?;

        let Some((_, MintEvent::Send(_))) = send_events.next().await else {
            panic!("Expected Send event");
        };

        let operation = client_receive.mint().receive(&ecash)?;

        let Some((op, MintEvent::Receive(_))) = receive_events.next().await else {
            panic!("Expected Receive event");
        };
        assert_eq!(op, operation);

        await_tx_outcome(&client_receive, operation)
            .await
            .expect("receive tx should be accepted");
    }

    info!("mint: send_and_receive passed");

    // Snapshot the receive client's accumulated balance now — *before* the
    // double-spend phase. The rejected receive runs `balance()`, which
    // opportunistically pulls excess notes (>TARGET_PER_DENOMINATION) into
    // the IssuanceSM's `spendable_notes` and only recovers them once the SM
    // transitions on Err. Capturing here avoids racing that recovery.
    let expected = client_receive.get_balance();

    ensure!(
        expected != Amount::ZERO,
        "client_receive should have a non-zero balance before recovery"
    );

    info!("mint: double_spend_is_rejected");

    let ecash = client_send.mint().send(Amount::from_sat(1_000)).await?;

    let Some((_, MintEvent::Send(_))) = send_events.next().await else {
        panic!("Expected Send event");
    };

    // First receive succeeds (sender receives own ecash back)
    let operation = client_send.mint().receive(&ecash)?;

    let Some((op, MintEvent::Receive(_))) = send_events.next().await else {
        panic!("Expected Receive event");
    };
    assert_eq!(op, operation);

    await_tx_outcome(client_send, operation)
        .await
        .expect("first receive should be accepted");

    // Second receive with same ecash is rejected
    let operation = client_receive.mint().receive(&ecash)?;

    let Some((op, MintEvent::Receive(_))) = receive_events.next().await else {
        panic!("Expected Receive event");
    };
    assert_eq!(op, operation);

    assert!(
        await_tx_outcome(&client_receive, operation).await.is_err(),
        "double-spend receive should be rejected",
    );

    info!("mint: double_spend_is_rejected passed");

    client_receive.shutdown().await;

    info!("mint: recovery (expected balance {expected})");

    let (recovered, recovery) = env.new_recovered_client(receive_mnemonic.clone()).await?;

    let scanned = recovery.amount();

    ensure!(
        scanned == expected,
        "recovery scanned {scanned}, expected {expected}"
    );

    // Second half of the restore: the scanned notes go back through the
    // ordinary out-of-band receive, so the wallet holds them only once that
    // reissuance settles. It re-mints under fresh outputs, leaving the balance
    // just below `expected` by the federation's fees.
    let operation = recovered.mint().receive(&recovery.ecash())?;

    await_tx_outcome(&recovered, operation)
        .await
        .expect("recovery reissuance should be accepted");

    let swept = recovered.get_balance();

    ensure!(
        swept > Amount::ZERO && swept <= expected,
        "recovered balance out of range: {swept} vs {expected}"
    );

    let loss = expected.checked_sub(swept).expect("swept <= expected");
    ensure!(
        loss < Amount::from_sat(50),
        "recovery lost more than expected to fees: {expected} -> {swept} (loss {loss})"
    );

    recovered.shutdown().await;

    info!("mint: recovery passed");

    // Recovering a second time is the only phase that exercises the counter
    // mark the first recovery persisted — the reissuance above re-mints the
    // whole wallet under counters past that mark. A mark one batch too high
    // opens a gap as wide as the one a scan refuses to cross, stranding every
    // reissued note behind it, and the wallet comes back empty rather than
    // merely short.
    info!("mint: second recovery (expected balance {swept})");

    let (recovered, recovery) = env.new_recovered_client(receive_mnemonic).await?;

    let scanned = recovery.amount();

    ensure!(
        scanned == swept,
        "second recovery scanned {scanned}, expected {swept}"
    );

    recovered.shutdown().await;

    info!("mint: second recovery passed");

    Ok(())
}
