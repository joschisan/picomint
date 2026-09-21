use anyhow::{Context as _, ensure};
use picomint_client::swap::events::{BrokerSuccessEvent, ReceiveEvent, SendSuccessEvent};
use picomint_core::Amount;
use tracing::info;

use crate::cli;
use crate::client::TestClient;
use crate::env::{NUM_ONLINE_NODES, TestEnv, retry};

const AMOUNT: bitcoin::Amount = bitcoin::Amount::from_sat(10_000);

pub async fn run_tests(env: &TestEnv, client_send: &TestClient) -> anyhow::Result<()> {
    test_direct(env, client_send).await?;

    register_broker(env)?;
    client_send.swap_broker_refresh()?;
    test_swap(env, client_send).await?;
    test_send_max(env, client_send).await?;
    deregister_broker(env)?;

    Ok(())
}

fn register_broker(env: &TestEnv) -> anyhow::Result<()> {
    for node in 0..NUM_ONLINE_NODES {
        let data_dir = cli::node_data_dir(&env.data_dir, node);
        cli::node_swap_broker_add(&data_dir, &env.broker_pk)?;
    }
    Ok(())
}

fn deregister_broker(env: &TestEnv) -> anyhow::Result<()> {
    for node in 0..NUM_ONLINE_NODES {
        let data_dir = cli::node_data_dir(&env.data_dir, node);
        cli::node_swap_broker_remove(&data_dir, &env.broker_pk)?;
    }
    Ok(())
}

/// The recipient's balance after claiming a receive contract: the amount
/// less the mint's per-input fee and the change outputs' fees, which is a
/// few sat at most.
fn assert_received(balance: Amount) -> anyhow::Result<()> {
    let amount = Amount::from_sat(AMOUNT.to_sat());

    ensure!(
        balance > amount - Amount::from_sat(10) && balance <= amount,
        "received balance {balance} is not the swap amount {amount} less fees"
    );

    Ok(())
}

/// A payment to an address of the sender's own mint is one receive
/// contract output, claimed by the recipient's stream scan.
async fn test_direct(env: &TestEnv, client_send: &TestClient) -> anyhow::Result<()> {
    info!("swap: direct payment within the mint");

    let client_receive = env.new_client().await?;

    let address = client_receive.swap_receive()?;

    let operation = client_send.swap_send_direct(&address, AMOUNT)?;

    client_send
        .await_tx_outcome(operation)
        .await?
        .expect("direct swap should be accepted");

    client_receive
        .await_event::<ReceiveEvent>(operation)
        .await?;

    let balance = retry("direct swap credited", || async {
        let balance = client_receive.balance()?;
        ensure!(balance > Amount::ZERO, "balance is zero");
        Ok(balance)
    })
    .await?;

    assert_received(balance)?;

    info!("swap: direct payment passed");

    Ok(())
}

/// A payment through the broker: the sender locks a send contract, the
/// broker funds the receive contract in the address's mint, collects its
/// attestation and claims the send contract with it, and the recipient's
/// scan claims the receive contract. The address lives in the mint the
/// payment leaves, so the round trip runs on one mint.
async fn test_swap(env: &TestEnv, client_send: &TestClient) -> anyhow::Result<()> {
    info!("swap: through the broker");

    let broker = client_send.swap_broker()?;

    ensure!(
        broker == env.broker_pk,
        "the mint recommends another broker"
    );

    let client_receive = env.new_client().await?;

    let address = client_receive.swap_receive()?;

    let operation = client_send.swap_send(broker, &address, AMOUNT)?;

    client_send
        .await_event::<SendSuccessEvent>(operation)
        .await?;

    client_receive
        .await_event::<ReceiveEvent>(operation)
        .await?;

    let balance = retry("swap credited", || async {
        let balance = client_receive.balance()?;
        ensure!(balance > Amount::ZERO, "balance is zero");
        Ok(balance)
    })
    .await?;

    assert_received(balance)?;

    // The broker's operation is its own, so its claim of the send contract
    // is found by the mint it landed in rather than by the sender's
    // operation.
    retry("broker claim logged", || async {
        let rows = cli::broker_query(
            &env.broker_data_dir,
            &format!(
                "SELECT * FROM {} WHERE mint = '{}'",
                picomint_analytics::table_name::<BrokerSuccessEvent>(),
                env.invite.mint
            ),
        )?;

        rows.into_iter()
            .next()
            .context("not logged yet")
            .map(|_| ())
    })
    .await?;

    info!("swap: through the broker passed");

    test_rebalance(env).await
}

/// A max send empties the account, directly and through the broker: the
/// sender is left with nothing, and the recipient gets what the sizing
/// promised.
async fn test_send_max(env: &TestEnv, client_send: &TestClient) -> anyhow::Result<()> {
    info!("swap: send max");

    let client_sender = env.new_client().await?;

    let operation = client_send.swap_send_direct(&client_sender.swap_receive()?, AMOUNT)?;

    client_sender.await_event::<ReceiveEvent>(operation).await?;

    let client_receive = env.new_client().await?;

    let amount = client_sender.swap_send_max_amount_direct()?;

    ensure!(amount > Amount::ZERO, "max direct amount is zero");

    let operation = client_sender.swap_send_max_direct(&client_receive.swap_receive()?)?;

    client_receive
        .await_event::<ReceiveEvent>(operation)
        .await?;

    ensure!(
        client_sender.balance()? == Amount::ZERO,
        "a max send should empty the account"
    );

    let balance = retry("max send credited", || async {
        let balance = client_receive.balance()?;
        ensure!(balance > Amount::ZERO, "balance is zero");
        Ok(balance)
    })
    .await?;

    ensure!(
        balance > amount - Amount::from_sat(10) && balance <= amount,
        "received balance {balance} is not the sized amount {amount} less fees"
    );

    let broker = env.broker_pk;

    let client_receive = env.new_client().await?;

    let client_sender = env.new_client().await?;

    let operation = client_send.swap_send_direct(&client_sender.swap_receive()?, AMOUNT)?;

    client_sender.await_event::<ReceiveEvent>(operation).await?;

    // A fresh client fills its broker info in the background after the
    // mint is added; the refresh makes the probe synchronous.
    client_sender.swap_broker_refresh()?;

    let amount = client_sender.swap_send_max_amount(broker)?;

    ensure!(
        amount > Amount::ZERO,
        "max amount through the broker is zero"
    );

    let operation = client_sender.swap_send_max(broker, &client_receive.swap_receive()?)?;

    client_sender
        .await_event::<SendSuccessEvent>(operation)
        .await?;

    client_receive
        .await_event::<ReceiveEvent>(operation)
        .await?;

    ensure!(
        client_sender.balance()? == Amount::ZERO,
        "a max send through the broker should empty the account"
    );

    let balance = retry("max send credited", || async {
        let balance = client_receive.balance()?;
        ensure!(balance > Amount::ZERO, "balance is zero");
        Ok(balance)
    })
    .await?;

    ensure!(
        balance > amount - Amount::from_sat(10) && balance <= amount,
        "received balance {balance} is not the sized amount {amount} less fees"
    );

    info!("swap: send max passed");

    Ok(())
}

/// A rebalance needs two mints to move between; with one it is refused
/// rather than a transfer to nowhere.
async fn test_rebalance(env: &TestEnv) -> anyhow::Result<()> {
    info!("swap: rebalance");

    ensure!(
        cli::broker_rebalance(&env.broker_data_dir).is_err(),
        "a rebalance with one mint should be refused"
    );

    info!("swap: rebalance passed");

    Ok(())
}
