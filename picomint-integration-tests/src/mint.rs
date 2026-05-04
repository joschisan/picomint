use std::pin::pin;
use std::sync::Arc;

use anyhow::ensure;
use async_stream::stream;
use futures::StreamExt;
use picomint_client::mint::{MintSuccessEvent, ReceiveEvent, RecoveryEvent, SendEvent};
use picomint_client::{Client, Mnemonic, TxAcceptEvent, TxRejectEvent};
use picomint_core::Amount;
use picomint_core::core::OperationId;
use picomint_eventlog::{EventLogEntry, EventLogId};
use tracing::info;

use crate::env::{TestEnv, retry};

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

/// Tail the global event log and yield only `RecoveryEvent` entries, in
/// log order. Used by the recovery test, which doesn't carry the
/// recovery operation id back from `Client::init_recovery`.
fn recovery_event_stream(client: &Arc<Client>) -> impl futures::Stream<Item = RecoveryEvent> {
    let client = client.clone();
    let notify = client.event_notify();
    let mut next_id = EventLogId::LOG_START;

    stream! {
        loop {
            let notified = notify.notified();
            let events = client.get_event_log(next_id, 100).await;

            for (id, entry) in events {
                next_id = id.saturating_add(1);

                if let Some(ev) = entry.to_event::<RecoveryEvent>() {
                    yield ev;
                }
            }

            notified.await;
        }
    }
}

/// Wait until a receive operation is fully settled. Returns:
/// - `Ok` once both `TxAcceptEvent` AND `MintSuccessEvent` have been
///   observed — at that point the spendable notes have been written
///   to the local NOTE table and the balance reflects the receive.
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
    let client_receive = env
        .new_client(Some(receive_mnemonic.clone()), false)
        .await?;

    let mut send_events = pin!(mint_event_stream(client_send));
    let mut receive_events = pin!(mint_event_stream(&client_receive));

    for i in 0..10 {
        info!("Sending ecash payment {} of 10", i + 1);

        let ecash = client_send.mint().send(Amount::from_sats(1_000)).await?;

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
    // opportunistically pulls excess notes (>2×TARGET_PER_DENOMINATION) into
    // the IssuanceSM's `spendable_notes` and only recovers them once the SM
    // transitions on Err. Capturing here avoids racing that recovery.
    let expected = client_receive.get_balance().await?;

    ensure!(
        expected != Amount::ZERO,
        "client_receive should have a non-zero balance before recovery"
    );

    info!("mint: double_spend_is_rejected");

    let ecash = client_send.mint().send(Amount::from_sats(1_000)).await?;

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

    // `init_recovery: true` seeds the recovery row in the same call
    // that opens the db, BEFORE `Client::new` runs — so the
    // constructor's presence check picks it up and spawns the driver.
    let recovered = env.new_client(Some(receive_mnemonic), true).await?;

    // Verify the event stream: first `RecoveryEvent` has
    // `total = None` (init_recovery couldn't know it without hitting
    // the network), and we eventually see a terminal one with
    // `index == total` once the driver finishes. We tail the global
    // event log here rather than `subscribe_operation_events` because
    // we don't carry the operation id back from `init_recovery`.
    let mut events = pin!(recovery_event_stream(&recovered));

    let first = events.next().await.expect("first recovery event");

    assert_eq!(first.index, 0);
    assert_eq!(first.total, None);

    loop {
        let ev = events.next().await.expect("client running");

        if ev.total.is_some_and(|total| ev.index == total) {
            break;
        }
    }

    // The terminal recovery event commits in the same dbtx as the
    // reissuance-tx submission. The tx still has to round-trip through
    // consensus and its mint state machine has to write fresh notes
    // before the balance reflects the recovered funds. Recovery
    // re-mints under fresh outputs, so the post-recovery balance is
    // slightly below `expected` due to mint fees on the reissuance.
    retry("recovery balance match", || async {
        let bal = recovered.get_balance().await?;

        ensure!(
            bal > Amount::ZERO && bal <= expected,
            "balance not yet positive: {bal} vs {expected}"
        );

        let loss = expected.checked_sub(bal).expect("bal <= expected");
        ensure!(
            loss < Amount::from_sats(50),
            "recovery lost more than expected to fees: {expected} -> {bal} (loss {loss})"
        );

        Ok(())
    })
    .await?;

    recovered.shutdown().await;

    info!("mint: recovery passed");

    Ok(())
}
