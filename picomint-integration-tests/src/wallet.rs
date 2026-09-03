use std::pin::pin;

use anyhow::{Context, ensure};
use async_stream::stream;
use bitcoincore_rpc::RpcApi;
use futures::StreamExt;
use picomint_client::eventlog::{EventLogEntry, EventLogId};
use picomint_client::wallet::events::{ReceiveEvent, SendEvent, SendSuccessEvent};
use picomint_client::{Account, TxRejectEvent};
use picomint_core::Amount;
use tokio::task::block_in_place;
use tracing::info;

use crate::env::{TestClient, TestEnv, retry};

#[derive(Debug)]
#[allow(dead_code)]
enum WalletEvent {
    Send(SendEvent),
    SendSuccess(SendSuccessEvent),
    Receive(ReceiveEvent),
    TxReject(TxRejectEvent),
}

fn wallet_event_stream(
    client: &TestClient,
) -> impl futures::Stream<Item = (picomint_core::core::OperationId, WalletEvent)> {
    let client = client.clone();
    let notify = client.client.event_notify();
    let mut next_id = EventLogId::LOG_START;

    stream! {
        loop {
            let notified = notify.notified();
            let events = client.client.get_event_log(next_id, 100);

            for (id, entry) in events {
                next_id = id.saturating_add(1);

                if let Some((op, event)) = try_parse_wallet_event(&entry) {
                    yield (op, event);
                }
            }

            notified.await;
        }
    }
}

fn try_parse_wallet_event(
    entry: &EventLogEntry,
) -> Option<(picomint_core::core::OperationId, WalletEvent)> {
    let op = entry.operation;
    if let Some(e) = entry.to_event() {
        return Some((op, WalletEvent::Send(e)));
    }
    if let Some(e) = entry.to_event() {
        return Some((op, WalletEvent::SendSuccess(e)));
    }
    if let Some(e) = entry.to_event() {
        return Some((op, WalletEvent::Receive(e)));
    }
    if let Some(e) = entry.to_event() {
        return Some((op, WalletEvent::TxReject(e)));
    }
    None
}

pub async fn run_tests(env: &TestEnv, client_send: &TestClient) -> anyhow::Result<()> {
    info!("wallet: pegin + on-chain send");

    let mut send_events = pin!(wallet_event_stream(client_send));

    let pegin_addr = retry("deposit address derived", || async {
        client_send
            .client
            .wallet_deposit_address(client_send.fed, Account::Primary)
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

    // Drain the wallet events emitted by the pegin itself.
    let Some((_, WalletEvent::Receive(_))) = send_events.next().await else {
        panic!("Expected pegin Receive event");
    };

    info!(addr = %pegin_addr, "Pegin Receive Event");

    retry("pegin balance", || async {
        let balance = client_send
            .client
            .mint_balance(client_send.fed, Account::Primary);
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
        .wallet_send(
            client_send.fed,
            Account::Primary,
            external_address.as_unchecked().clone(),
            bitcoin::Amount::from_sat(100_000),
            None,
        )
        .await?;

    let Some((op, WalletEvent::Send(_))) = send_events.next().await else {
        panic!("Expected Send event");
    };
    assert_eq!(op, operation);

    let Some((op, WalletEvent::SendSuccess(ev))) = send_events.next().await else {
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

    info!("wallet: pegin + on-chain send passed");

    info!("wallet: zero_fee_send_aborts");

    let abort_op = client_send
        .client
        .wallet_send(
            client_send.fed,
            Account::Primary,
            external_address.as_unchecked().clone(),
            bitcoin::Amount::from_sat(100_000),
            Some(bitcoin::Amount::ZERO),
        )
        .await?;

    let Some((op, WalletEvent::Send(_))) = send_events.next().await else {
        panic!("Expected Send event");
    };
    assert_eq!(op, abort_op);

    let Some((op, WalletEvent::TxReject(_))) = send_events.next().await else {
        panic!("Expected TxReject event");
    };
    assert_eq!(op, abort_op);

    info!("wallet: zero_fee_send_aborts passed");

    info!("wallet: send_max leaves no notes");

    // A fresh client, so emptying the account cannot interfere with the
    // suites that draw on `client_send` afterwards.
    let client = env.new_client(None).await?;

    let ecash = client_send
        .client
        .mint_send(client_send.fed, Account::Primary, Amount::from_sat(100_000))
        .await?;

    let operation = client
        .client
        .mint_receive(client.fed, Account::Primary, &ecash)?;

    crate::mint::await_tx_outcome(&client, operation)
        .await
        .expect("funding receive should be accepted");

    let amount = client
        .client
        .wallet_send_max_amount(client.fed, Account::Primary)
        .await?;

    ensure!(amount > bitcoin::Amount::ZERO, "max send amount is zero");

    let mut events = pin!(wallet_event_stream(&client));

    let operation = client
        .client
        .wallet_send_max(
            client.fed,
            Account::Primary,
            external_address.as_unchecked().clone(),
        )
        .await?;

    let Some((op, WalletEvent::Send(_))) = events.next().await else {
        panic!("Expected Send event");
    };
    assert_eq!(op, operation);

    let Some((op, WalletEvent::SendSuccess(_))) = events.next().await else {
        panic!("Expected SendSuccess event");
    };
    assert_eq!(op, operation);

    ensure!(
        client
            .client
            .mint_count_by_denomination(client.fed, Account::Primary)
            .is_empty(),
        "send_max left notes behind"
    );

    client.client.shutdown().await;

    info!("wallet: send_max passed");

    info!("wallet: second pegin sweeps the deposit and the federation utxo");

    let pegin_addr = retry("second deposit address derived", || async {
        client_send
            .client
            .wallet_deposit_address(client_send.fed, Account::Primary)
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

    let Some((_, WalletEvent::Receive(_))) = send_events.next().await else {
        panic!("Expected second pegin Receive event");
    };

    // Unlike the first pegin, whose deposit simply becomes the federation
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

    info!("wallet: second pegin sweep passed");

    Ok(())
}
