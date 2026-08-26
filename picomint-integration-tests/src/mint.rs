use std::pin::pin;
use std::sync::Arc;

use anyhow::ensure;
use async_stream::stream;
use futures::StreamExt;
use picomint_client::mint::{MintSuccessEvent, ReceiveEvent, SendEvent};
use picomint_client::{Account, Client, Mnemonic, TxAcceptEvent, TxRejectEvent};
use picomint_core::Amount;
use picomint_core::core::OperationId;
use picomint_eventlog::{EventLogEntry, EventLogId};
use tracing::info;

use crate::env::{CLIENT_FEE_PPM, TestEnv};

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
            let events = client.get_event_log(next_id, 100);

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

/// Wait for the reissuance a freshly-joined client's restore staged, and
/// return what the scan found — the gross amount, before the reissuance's
/// fees.
///
/// Only sound on a client whose database is new, since it takes the first
/// receive the log ever carried. Panics if the reissuance is rejected: a
/// client built on a scan that turned something up has nothing else to
/// receive, so there is no later attempt to wait for.
async fn await_restore(client: &Arc<Client>) -> Amount {
    let mut stream = pin!(mint_event_stream(client));

    loop {
        match stream.next().await {
            Some((operation, MintEvent::Receive(event))) => {
                await_tx_outcome(client, operation)
                    .await
                    .expect("restore reissuance should be accepted");

                break event.amount;
            }
            Some(_) => continue,
            None => unreachable!("stream only ends at client shutdown"),
        }
    }
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

    // Capture the receive client's mnemonic so we can restore it at
    // the end of the suite, after it has accumulated a balance.
    let receive_mnemonic = Mnemonic::generate(12)?;
    let client_receive = env.new_client(Some(receive_mnemonic.clone())).await?;

    let mut send_events = pin!(mint_event_stream(client_send));
    let mut receive_events = pin!(mint_event_stream(&client_receive));

    for i in 0..10 {
        info!("Sending ecash payment {} of 10", i + 1);

        let ecash = client_send
            .mint()
            .send(Account::PRIMARY, Amount::from_sat(1_000))
            .await?;

        let Some((_, MintEvent::Send(_))) = send_events.next().await else {
            panic!("Expected Send event");
        };

        let operation = client_receive.mint().receive(Account::PRIMARY, &ecash)?;

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
    // transitions on Err. Capturing here avoids racing that reclaim.
    let expected = client_receive.get_balance(Account::PRIMARY);

    ensure!(
        expected != Amount::ZERO,
        "client_receive should have a non-zero balance before restore"
    );

    info!("mint: double_spend_is_rejected");

    let ecash = client_send
        .mint()
        .send(Account::PRIMARY, Amount::from_sat(1_000))
        .await?;

    let Some((_, MintEvent::Send(_))) = send_events.next().await else {
        panic!("Expected Send event");
    };

    // First receive succeeds (sender receives own ecash back)
    let operation = client_send.mint().receive(Account::PRIMARY, &ecash)?;

    let Some((op, MintEvent::Receive(_))) = send_events.next().await else {
        panic!("Expected Receive event");
    };
    assert_eq!(op, operation);

    await_tx_outcome(client_send, operation)
        .await
        .expect("first receive should be accepted");

    // Second receive with same ecash is rejected
    let operation = client_receive.mint().receive(Account::PRIMARY, &ecash)?;

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

    info!("mint: restore (expected balance {expected})");

    let restored = env.new_client(Some(receive_mnemonic.clone())).await?;

    // Restoring is not its own entry point: the scan happened inside the join
    // and the notes went back through the ordinary out-of-band receive as the
    // client came up, so what the scan found is observed through the receive
    // it produced rather than returned to the caller.
    let scanned = await_restore(&restored).await;

    ensure!(
        scanned == expected,
        "restore scanned {scanned}, expected {expected}"
    );

    // The reissuance re-mints under fresh outputs, so the wallet holds the
    // balance only once it settles, just below `expected` by the federation's
    // fees.
    let swept = restored.get_balance(Account::PRIMARY);

    ensure!(
        swept > Amount::ZERO && swept <= expected,
        "restored balance out of range: {swept} vs {expected}"
    );

    // The reissuance pays the federation for its outputs and the integrator
    // its cut of what it claimed, so the cut is what the bound is made of and
    // the federation's fees are the allowance on top of it.
    let cut = Amount::from_msat(expected.msat * CLIENT_FEE_PPM / 1_000_000);

    let loss = expected.checked_sub(swept).expect("swept <= expected");
    ensure!(
        loss < cut + Amount::from_sat(50),
        "restore lost more than expected to fees: {expected} -> {swept} (loss {loss})"
    );

    restored.shutdown().await;

    info!("mint: restore passed");

    // Restoring a second time is the only phase that exercises the counter
    // mark the first restore persisted — the reissuance above re-mints the
    // whole wallet under counters past that mark. A mark one batch too high
    // opens a gap as wide as the one a scan refuses to cross, stranding every
    // reissued note behind it, and the wallet comes back empty rather than
    // merely short.
    info!("mint: second restore (expected balance {swept})");

    let restored = env.new_client(Some(receive_mnemonic)).await?;

    let scanned = await_restore(&restored).await;

    ensure!(
        scanned == swept,
        "second restore scanned {scanned}, expected {swept}"
    );

    restored.shutdown().await;

    info!("mint: second restore passed");

    Ok(())
}
