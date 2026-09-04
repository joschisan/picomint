use std::pin::pin;

use anyhow::{Context, ensure};
use async_stream::stream;
use bitcoincore_rpc::RpcApi;
use futures::StreamExt;
use picomint_client::eventlog::{EventLogEntry, EventLogId};
use picomint_client::onchain::events::{ReceiveEvent, SendEvent, SendSuccessEvent};
use picomint_client::{Account, TxRejectEvent};
use picomint_core::Amount;
use tokio::task::block_in_place;
use tracing::info;

use crate::env::{TestClient, TestEnv, retry};

#[derive(Debug)]
#[allow(dead_code)]
enum OnchainEvent {
    Send(SendEvent),
    SendSuccess(SendSuccessEvent),
    Receive(ReceiveEvent),
    TxReject(TxRejectEvent),
}

fn onchain_event_stream(
    client: &TestClient,
) -> impl futures::Stream<Item = (picomint_core::core::OperationId, OnchainEvent)> {
    let client = client.clone();
    let notify = client.client.event_notify();
    let mut next_id = EventLogId::LOG_START;

    stream! {
        loop {
            let notified = notify.notified();
            let events = client.client.get_event_log(next_id, 100);

            for (id, entry) in events {
                next_id = id.saturating_add(1);

                if let Some((op, event)) = try_parse_onchain_event(&entry) {
                    yield (op, event);
                }
            }

            notified.await;
        }
    }
}

fn try_parse_onchain_event(
    entry: &EventLogEntry,
) -> Option<(picomint_core::core::OperationId, OnchainEvent)> {
    let op = entry.operation;
    if let Some(e) = entry.to_event() {
        return Some((op, OnchainEvent::Send(e)));
    }
    if let Some(e) = entry.to_event() {
        return Some((op, OnchainEvent::SendSuccess(e)));
    }
    if let Some(e) = entry.to_event() {
        return Some((op, OnchainEvent::Receive(e)));
    }
    if let Some(e) = entry.to_event() {
        return Some((op, OnchainEvent::TxReject(e)));
    }
    None
}

pub async fn run_tests(env: &TestEnv, client_send: &TestClient) -> anyhow::Result<()> {
    info!("onchain: pegin + on-chain send");

    let mut send_events = pin!(onchain_event_stream(client_send));

    let pegin_addr = retry("deposit address derived", || async {
        client_send
            .client
            .onchain_receive(client_send.mint, Account::Primary)
    })
    .await?;
    info!(addr = %pegin_addr, "Pegin address ready");

    let pegin_txid = env.send_to_address(&pegin_addr, bitcoin::Amount::from_sat(100_000_000))?;

    retry("pegin tx in mempool", || async {
        block_in_place(|| env.bitcoind.get_mempool_entry(&pegin_txid))
            .map(|_| ())
            .context("pegin tx not in mempool yet")
    })
    .await?;

    env.mine_blocks(10);

    // Drain the onchain events emitted by the pegin itself.
    let Some((_, OnchainEvent::Receive(_))) = send_events.next().await else {
        panic!("Expected pegin Receive event");
    };

    info!(addr = %pegin_addr, "Pegin Receive Event");

    retry("pegin balance", || async {
        let balance = client_send
            .client
            .ecash_balance(client_send.mint, Account::Primary);
        ensure!(balance > Amount::ZERO, "Balance is zero");
        Ok(())
    })
    .await?;

    info!(addr = %pegin_addr, "Pegin Balance is available");

    // A taproot destination has the largest scriptPubKey the wallet pays
    // (34 bytes), so this send exercises the worst-case transaction size the
    // fee constants are derived from.
    let external_address = block_in_place(|| {
        env.bitcoind
            .get_new_address(None, Some(bitcoincore_rpc::json::AddressType::Bech32m))
    })?
    .require_network(bitcoin::Network::Regtest)?;

    info!(address = %external_address, "Sending on-chain to external address");

    let operation = client_send
        .client
        .onchain_send(
            client_send.mint,
            Account::Primary,
            external_address.as_unchecked().clone(),
            bitcoin::Amount::from_sat(100_000),
            None,
        )
        .await?;

    let Some((op, OnchainEvent::Send(_))) = send_events.next().await else {
        panic!("Expected Send event");
    };
    assert_eq!(op, operation);

    let Some((op, OnchainEvent::SendSuccess(ev))) = send_events.next().await else {
        panic!("Expected SendSuccess event");
    };
    assert_eq!(op, operation);
    let txid = ev.txid;

    info!(%txid, "Send confirmed, waiting for tx broadcast");

    // The background miner may confirm the peg-out out of the mempool
    // between polls, and bitcoind runs without txindex — so probe the
    // (still-unspent right after broadcast) peg-out outputs for the
    // confirmed case.
    retry("send tx broadcast", || async {
        let in_mempool = block_in_place(|| env.bitcoind.get_mempool_entry(&txid)).is_ok();

        let confirmed = (0..2).any(|vout| {
            block_in_place(|| env.bitcoind.get_tx_out(&txid, vout, Some(false)))
                .ok()
                .flatten()
                .is_some()
        });

        ensure!(
            in_mempool || confirmed,
            "send tx neither in mempool nor confirmed"
        );

        Ok(())
    })
    .await?;

    info!("onchain: pegin + on-chain send passed");

    info!("onchain: zero_fee_send_aborts");

    let abort_op = client_send
        .client
        .onchain_send(
            client_send.mint,
            Account::Primary,
            external_address.as_unchecked().clone(),
            bitcoin::Amount::from_sat(100_000),
            Some(bitcoin::Amount::ZERO),
        )
        .await?;

    let Some((op, OnchainEvent::Send(_))) = send_events.next().await else {
        panic!("Expected Send event");
    };
    assert_eq!(op, abort_op);

    let Some((op, OnchainEvent::TxReject(_))) = send_events.next().await else {
        panic!("Expected TxReject event");
    };
    assert_eq!(op, abort_op);

    info!("onchain: zero_fee_send_aborts passed");

    info!("onchain: send_max leaves no notes");

    // A fresh client, so emptying the account cannot interfere with the
    // suites that draw on `client_send` afterwards.
    let client = env.new_client(None).await?;

    let ecash = client_send
        .client
        .ecash_send(
            client_send.mint,
            Account::Primary,
            Amount::from_sat(100_000),
        )
        .await?;

    let operation = client
        .client
        .ecash_receive(client.mint, Account::Primary, &ecash)?;

    crate::ecash::await_tx_outcome(&client, operation)
        .await
        .expect("funding receive should be accepted");

    let amount = client
        .client
        .onchain_send_max_amount(client.mint, Account::Primary)
        .await?;

    ensure!(amount > bitcoin::Amount::ZERO, "max send amount is zero");

    let mut events = pin!(onchain_event_stream(&client));

    let operation = client
        .client
        .onchain_send_max(
            client.mint,
            Account::Primary,
            external_address.as_unchecked().clone(),
        )
        .await?;

    let Some((op, OnchainEvent::Send(_))) = events.next().await else {
        panic!("Expected Send event");
    };
    assert_eq!(op, operation);

    let Some((op, OnchainEvent::SendSuccess(_))) = events.next().await else {
        panic!("Expected SendSuccess event");
    };
    assert_eq!(op, operation);

    ensure!(
        client
            .client
            .ecash_count(client.mint, Account::Primary)
            .is_empty(),
        "send_max left notes behind"
    );

    client.client.shutdown().await;

    info!("onchain: send_max passed");

    info!("onchain: second pegin sweeps the deposit and the mint utxo");

    let pegin_addr = retry("second deposit address derived", || async {
        client_send
            .client
            .onchain_receive(client_send.mint, Account::Primary)
    })
    .await?;

    let pegin_txid = env.send_to_address(&pegin_addr, bitcoin::Amount::from_sat(100_000_000))?;

    retry("second pegin tx in mempool", || async {
        block_in_place(|| env.bitcoind.get_mempool_entry(&pegin_txid))
            .map(|_| ())
            .context("second pegin tx not in mempool yet")
    })
    .await?;

    env.mine_blocks(10);

    // Locate the deposit output before the claim can spend it.
    let deposit_vout = (0..2)
        .find(|vout| {
            block_in_place(|| env.bitcoind.get_tx_out(&pegin_txid, *vout, Some(false)))
                .ok()
                .flatten()
                .is_some_and(|out| {
                    out.script_pub_key.hex == pegin_addr.script_pubkey().into_bytes()
                })
        })
        .expect("the deposit output pays the pegin address");

    let Some((_, OnchainEvent::Receive(_))) = send_events.next().await else {
        panic!("Expected second pegin Receive event");
    };

    // Unlike the first pegin, whose deposit simply becomes the mint
    // wallet, this claim creates the two-input sweep transaction. The deposit
    // utxo leaving the confirmed set proves the signing session completed and
    // the sweep was broadcast and mined.
    retry("sweep tx confirmed", || async {
        ensure!(
            block_in_place(|| env
                .bitcoind
                .get_tx_out(&pegin_txid, deposit_vout, Some(false)))?
            .is_none(),
            "deposit utxo not swept yet"
        );

        Ok(())
    })
    .await?;

    info!("onchain: second pegin sweep passed");

    Ok(())
}
