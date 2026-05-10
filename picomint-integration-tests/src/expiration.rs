//! Integration test for the federation expiration announcement: each
//! guardian sets the same `(date, successor)` pair via the admin CLI; a
//! fresh client then fetches the announcement via threshold consensus
//! and surfaces it through `Client::expiration_status`.

use anyhow::ensure;
use picomint_core::expiration::ExpirationStatus;
use tracing::info;

use crate::cli;
use crate::env::{NUM_GUARDIANS, TestEnv};

pub async fn run_test(env: &TestEnv) -> anyhow::Result<()> {
    info!("expiration: announce + client refresh");

    // Use the federation's own invite code as the successor — this is just
    // a value the guardians have to agree on byte-for-byte. A real
    // deployment would point at a successor federation; here we want the
    // successor field exercised end-to-end.
    let timestamp = 4_102_444_800; // 2100-01-01 UTC

    info!("Setting expiration on all {NUM_GUARDIANS} guardians");
    let expected = ExpirationStatus {
        timestamp,
        successor: Some(env.invite_code.clone()),
    };
    for peer in 0..NUM_GUARDIANS {
        let data_dir = cli::guardian_data_dir(&env.data_dir, peer);
        cli::guardian_expiration_set(&data_dir, timestamp, Some(&env.invite_code))?;
        let stored = cli::guardian_expiration_status(&data_dir)?;
        ensure!(
            stored.as_ref() == Some(&expected),
            "guardian {peer} stored expiration mismatch: got {stored:?}"
        );
    }

    // Spin up a fresh client so the cache starts empty.
    let client = env.new_client(None, false).await?;

    // The startup refresh task races with us; force a sync read so the
    // cache is settled before we assert.
    picomint_client::Client::refresh_expiration_status(client.clone())
        .await
        .map_err(|e| anyhow::anyhow!("refresh_expiration_status: {e}"))?;

    let cached = client
        .expiration_status()
        .ok_or_else(|| anyhow::anyhow!("expected client cache to hold the announcement"))?;

    ensure!(
        cached == expected,
        "client expiration mismatch: got {cached:?}, want {expected:?}"
    );

    info!("Clearing expiration on all guardians");
    for peer in 0..NUM_GUARDIANS {
        let data_dir = cli::guardian_data_dir(&env.data_dir, peer);
        cli::guardian_expiration_clear(&data_dir)?;
    }

    picomint_client::Client::refresh_expiration_status(client.clone())
        .await
        .map_err(|e| anyhow::anyhow!("refresh_expiration_status (clear): {e}"))?;

    ensure!(
        client.expiration_status().is_none(),
        "client cache should be empty after a federation-wide clear"
    );

    info!("expiration: passed");
    Ok(())
}
