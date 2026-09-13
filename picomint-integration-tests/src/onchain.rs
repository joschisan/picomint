use anyhow::{Context, ensure};
use bitcoincore_rpc::RpcApi;
use picomint_client::TxRejectEvent;
use picomint_client::onchain::events::{ReceiveEvent, SendSuccessEvent};
use picomint_core::Amount;
use tokio::task::block_in_place;
use tracing::info;

use crate::client::TestClient;
use crate::env::{TestEnv, retry};

/// Wait for the client to log the claim of a deposit to `address`.
async fn await_receive(client: &TestClient, address: &bitcoin::Address) -> anyhow::Result<()> {
    client
        .await_row::<ReceiveEvent>(&format!("address = '{address}'"))
        .await
        .map(|_| ())
}

pub async fn run_tests(env: &TestEnv, client_send: &TestClient) -> anyhow::Result<()> {
    info!("onchain: pegin + on-chain send");

    let pegin_addr = retry("deposit address derived", || async {
        client_send.onchain_receive()
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

    await_receive(client_send, &pegin_addr).await?;

    info!(addr = %pegin_addr, "Pegin Receive Event");

    retry("pegin balance", || async {
        let balance = client_send.balance()?;
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

    let operation =
        client_send.onchain_send(&external_address, bitcoin::Amount::from_sat(100_000), None)?;

    let txid = client_send
        .await_event::<SendSuccessEvent>(operation)
        .await?["txid"]
        .as_str()
        .context("txid column")?
        .parse::<bitcoin::Txid>()?;

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

    let abort_op = client_send.onchain_send(
        &external_address,
        bitcoin::Amount::from_sat(100_000),
        Some(bitcoin::Amount::ZERO),
    )?;

    client_send.await_event::<TxRejectEvent>(abort_op).await?;

    info!("onchain: zero_fee_send_aborts passed");

    info!("onchain: send_max leaves no notes");

    // A fresh client, so emptying the account cannot interfere with the
    // suites that draw on `client_send` afterwards.
    let client = env.new_client().await?;

    let ecash = client_send.ecash_send(bitcoin::Amount::from_sat(100_000))?;

    let operation = client.ecash_receive(&ecash)?;

    client
        .await_tx_outcome(operation)
        .await?
        .expect("funding receive should be accepted");

    let amount = client.onchain_send_max_amount()?;

    ensure!(amount > bitcoin::Amount::ZERO, "max send amount is zero");

    let operation = client.onchain_send_max(&external_address)?;

    client.await_event::<SendSuccessEvent>(operation).await?;

    ensure!(
        client.ecash_count()?.is_empty(),
        "send_max left notes behind"
    );

    client.shutdown().await;

    info!("onchain: send_max passed");

    info!("onchain: second pegin sweeps the deposit and the mint utxo");

    let pegin_addr = retry("second deposit address derived", || async {
        client_send.onchain_receive()
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

    await_receive(client_send, &pegin_addr).await?;

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
