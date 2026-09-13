use anyhow::ensure;
use picomint_core::Amount;
use tracing::info;

use crate::client::TestClient;
use crate::env::TestEnv;

pub async fn run_tests(env: &TestEnv, client_send: &TestClient) -> anyhow::Result<()> {
    info!("ecash: send_and_receive (10 iterations) + double_spend_is_rejected");

    let client_receive = env.new_client().await?;

    for i in 0..10 {
        info!("Sending ecash payment {} of 10", i + 1);

        let ecash = client_send.ecash_send(bitcoin::Amount::from_sat(1_000))?;

        let operation = client_receive.ecash_receive(&ecash)?;

        client_receive
            .await_tx_outcome(operation)
            .await?
            .expect("receive tx should be accepted");
    }

    info!("ecash: send_and_receive passed");

    // Snapshot the receive client's accumulated balance now — *before* the
    // double-spend phase. The rejected receive runs `balance()`, which
    // opportunistically pulls excess notes (>TARGET_PER_DENOMINATION) into
    // the IssuanceSM's `spendable_notes` and only recovers them once the SM
    // transitions on Err. Capturing here avoids racing that reclaim.
    let expected = client_receive.balance()?;

    ensure!(
        expected != Amount::ZERO,
        "client_receive should have a non-zero balance before restore"
    );

    info!("ecash: double_spend_is_rejected");

    let ecash = client_send.ecash_send(bitcoin::Amount::from_sat(1_000))?;

    // First receive succeeds (sender receives own ecash back)
    let operation = client_send.ecash_receive(&ecash)?;

    client_send
        .await_tx_outcome(operation)
        .await?
        .expect("first receive should be accepted");

    // Second receive with same ecash is rejected
    let operation = client_receive.ecash_receive(&ecash)?;

    ensure!(
        client_receive.await_tx_outcome(operation).await?.is_err(),
        "double-spend receive should be rejected",
    );

    info!("ecash: double_spend_is_rejected passed");

    info!("ecash: restore (expected balance {expected})");

    // Restoring is not its own entry point and costs nothing: the scan runs
    // inside the join and writes its notes with the counter marks, so the
    // wallet is whole the moment the mint is added rather than once a
    // reissuance settles.
    client_receive.rejoin(&env.invite)?;

    let scanned = client_receive.balance()?;

    ensure!(
        scanned == expected,
        "restore scanned {scanned}, expected {expected}"
    );

    info!("ecash: restore passed");

    // Restoring writes no counters of its own, so the restored wallet has to
    // issue past the mark before a second restore means anything. Sending a
    // bundle and receiving it back re-mints under counters above the mark,
    // which is the state the next scan has to cross to.
    let ecash = client_receive.ecash_send(bitcoin::Amount::from_sat(1_000))?;

    let operation = client_receive.ecash_receive(&ecash)?;

    client_receive
        .await_tx_outcome(operation)
        .await?
        .expect("self-reissue should be accepted");

    let swept = client_receive.balance()?;

    ensure!(
        swept > Amount::ZERO && swept < expected,
        "reissue left balance out of range: {swept} vs {expected}"
    );

    // The send and the reissue pay the mint a fee per note in and out,
    // priced in msat, so a flat allowance hundreds of notes wide is still
    // tight enough to catch fees running away.
    let loss = expected.checked_sub(swept).expect("swept < expected");
    ensure!(
        loss < Amount::from_sat(50),
        "reissue lost more than expected to fees: {expected} -> {swept} (loss {loss})"
    );

    // Restoring a second time is the only phase that exercises the counter
    // mark the first restore persisted. A mark one batch too high opens a gap
    // as wide as the one a scan refuses to cross, stranding every note the
    // reissue issued behind it, and the wallet comes back empty rather than
    // merely short.
    info!("ecash: second restore (expected balance {swept})");

    client_receive.rejoin(&env.invite)?;

    let scanned = client_receive.balance()?;

    ensure!(
        scanned == swept,
        "second restore scanned {scanned}, expected {swept}"
    );

    client_receive.shutdown().await;

    info!("ecash: second restore passed");

    info!("ecash: send_max leaves no notes");

    // A fresh client, so emptying the account cannot race the lightning suite
    // running in parallel on `client_send`.
    let client = env.new_client().await?;

    let ecash = client_send.ecash_send(bitcoin::Amount::from_sat(5_000))?;

    let operation = client.ecash_receive(&ecash)?;

    client
        .await_tx_outcome(operation)
        .await?
        .expect("funding receive should be accepted");

    let ecash = client.ecash_send_max()?.expect("account holds notes");

    ensure!(
        client.ecash_count()?.is_empty(),
        "send_max left notes behind"
    );

    // The bundle is real value, so hand it back rather than burning it.
    let operation = client_send.ecash_receive(&ecash)?;

    client_send
        .await_tx_outcome(operation)
        .await?
        .expect("return receive should be accepted");

    client.shutdown().await;

    info!("ecash: send_max passed");

    Ok(())
}
