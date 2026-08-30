use std::pin::pin;

use anyhow::ensure;
use async_stream::stream;
use futures::StreamExt;
use picomint_client::mint::{MintSuccessEvent, ReceiveEvent, SendEvent};
use picomint_client::{Account, Mnemonic, TxAcceptEvent, TxRejectEvent};
use picomint_core::Amount;
use picomint_core::core::OperationId;
use picomint_eventlog::{EventLogEntry, EventLogId};
use tracing::info;

use crate::env::{CLIENT_FEE_PPM, TestClient, TestEnv};

#[derive(Debug)]
#[allow(dead_code)]
enum MintEvent {
    Send(SendEvent),
    Receive(ReceiveEvent),
}

fn mint_event_stream(client: &TestClient) -> impl futures::Stream<Item = (OperationId, MintEvent)> {
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

/// Consume `events` until one matches `predicate`, discarding the rest.
/// The ln and wallet suites fund their send_max clients with ecash from the
/// shared `client_send`, so its stream carries Send events that are not this
/// suite's — a strict next-event assertion would trip over them.
async fn wait_mint_event<S>(
    events: &mut std::pin::Pin<&mut S>,
    predicate: impl Fn(OperationId, &MintEvent) -> bool,
) where
    S: futures::Stream<Item = (OperationId, MintEvent)>,
{
    loop {
        let Some((op, event)) = events.next().await else {
            panic!("event stream ended");
        };

        if predicate(op, &event) {
            return;
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
pub(crate) async fn await_tx_outcome(
    client: &TestClient,
    operation: OperationId,
) -> Result<(), String> {
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

pub async fn run_tests(env: &TestEnv, client_send: &TestClient) -> anyhow::Result<()> {
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

        wait_mint_event(&mut send_events, |_, e| matches!(e, MintEvent::Send(_))).await;

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

    wait_mint_event(&mut send_events, |_, e| matches!(e, MintEvent::Send(_))).await;

    // First receive succeeds (sender receives own ecash back)
    let operation = client_send.mint().receive(Account::PRIMARY, &ecash)?;

    wait_mint_event(&mut send_events, |op, e| {
        op == operation && matches!(e, MintEvent::Receive(_))
    })
    .await;

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

    // Restoring is not its own entry point and costs nothing: the scan ran
    // inside the join and wrote its notes with the counter marks, so the
    // wallet is whole the moment the client opens rather than once a
    // reissuance settles.
    let scanned = restored.get_balance(Account::PRIMARY);

    ensure!(
        scanned == expected,
        "restore scanned {scanned}, expected {expected}"
    );

    info!("mint: restore passed");

    // Restoring writes no counters of its own, so the restored wallet has to
    // issue past the mark before a second restore means anything. Sending a
    // bundle and receiving it back re-mints under counters above the mark,
    // which is the state the next scan has to cross to.
    let ecash = restored
        .mint()
        .send(Account::PRIMARY, Amount::from_sat(1_000))
        .await?;

    let operation = restored.mint().receive(Account::PRIMARY, &ecash)?;

    await_tx_outcome(&restored, operation)
        .await
        .expect("self-remint should be accepted");

    let swept = restored.get_balance(Account::PRIMARY);

    ensure!(
        swept > Amount::ZERO && swept < expected,
        "remint left balance out of range: {swept} vs {expected}"
    );

    // The remint pays the federation for its outputs and the integrator its
    // cut of what moved, so the cut over the whole balance is the loosest
    // bound that still catches fees running away.
    let cut = Amount::from_msat(expected.msat * CLIENT_FEE_PPM / 1_000_000);

    let loss = expected.checked_sub(swept).expect("swept < expected");
    ensure!(
        loss < cut + Amount::from_sat(50),
        "remint lost more than expected to fees: {expected} -> {swept} (loss {loss})"
    );

    restored.shutdown().await;

    // Restoring a second time is the only phase that exercises the counter
    // mark the first restore persisted. A mark one batch too high opens a gap
    // as wide as the one a scan refuses to cross, stranding every note the
    // remint issued behind it, and the wallet comes back empty rather than
    // merely short.
    info!("mint: second restore (expected balance {swept})");

    let restored = env.new_client(Some(receive_mnemonic)).await?;

    let scanned = restored.get_balance(Account::PRIMARY);

    ensure!(
        scanned == swept,
        "second restore scanned {scanned}, expected {swept}"
    );

    restored.shutdown().await;

    info!("mint: second restore passed");

    info!("mint: send_max leaves no notes");

    // A fresh client, so emptying the account cannot race the ln suite
    // running in parallel on `client_send`.
    let client = env.new_client(None).await?;

    let ecash = client_send
        .mint()
        .send(Account::PRIMARY, Amount::from_sat(5_000))
        .await?;

    let operation = client.mint().receive(Account::PRIMARY, &ecash)?;

    await_tx_outcome(&client, operation)
        .await
        .expect("funding receive should be accepted");

    let ecash = client
        .mint()
        .send_max(Account::PRIMARY)
        .expect("account holds notes");

    ensure!(
        client
            .mint()
            .get_count_by_denomination(Account::PRIMARY)
            .is_empty(),
        "send_max left notes behind"
    );

    // The bundle is real value, so hand it back rather than burning it.
    let operation = client_send.mint().receive(Account::PRIMARY, &ecash)?;

    await_tx_outcome(client_send, operation)
        .await
        .expect("return receive should be accepted");

    client.shutdown().await;

    info!("mint: send_max passed");

    Ok(())
}
