//! Integration test for the mint expiry announcement: each node sets the
//! same date and successor via the admin CLI; a fresh client then fetches
//! the announcement via threshold consensus and surfaces it through its
//! `expiry` command.

use anyhow::ensure;
use chrono::{Days, Utc};
use picomint_core::expiry::ExpiryStatus;
use tracing::info;

use crate::cli;
use crate::env::{NUM_ONLINE_NODES, TestEnv};

pub async fn run_test(env: &TestEnv) -> anyhow::Result<()> {
    info!("expiry: announce + client refresh");

    // Use the mint's own invite code as the successor — this is just
    // a value the nodes have to agree on byte-for-byte. A real
    // deployment would point at a successor mint; here we want the
    // successor field exercised end-to-end.
    let date = Utc::now()
        .date_naive()
        .checked_add_days(Days::new(365))
        .expect("a year from today is within chrono's range");

    let expected = ExpiryStatus {
        timestamp: date
            .and_hms_opt(0, 0, 0)
            .expect("midnight exists on every day")
            .and_utc()
            .timestamp()
            .cast_unsigned(),
        successor: Some(env.invite.clone()),
    };

    let node0 = cli::node_data_dir(&env.data_dir, 0);

    ensure!(
        cli::node_expiry_set(&node0, "2000-01-01", None).is_err(),
        "a date in the past must be refused"
    );

    ensure!(
        cli::node_expiry_set(&node0, "2100-01-01", None).is_err(),
        "a date beyond the horizon must be refused"
    );

    ensure!(
        cli::node_expiry_set(&node0, "next year", None).is_err(),
        "a date that is not YYYY-MM-DD must be refused"
    );

    info!(
        "Setting expiry {} on all {NUM_ONLINE_NODES} online nodes",
        date
    );
    for node in 0..NUM_ONLINE_NODES {
        let data_dir = cli::node_data_dir(&env.data_dir, node);
        cli::node_expiry_set(&data_dir, &date.to_string(), Some(&env.invite))?;
        let stored = cli::node_expiry_status(&data_dir)?;
        ensure!(
            stored.as_ref() == Some(&expected),
            "node {node} stored expiry mismatch: got {stored:?}"
        );
    }

    // Spin up a fresh client so the cache starts empty; `expiry` refreshes
    // it synchronously before answering.
    let client = env.new_client().await?;

    let announced = client.expiry()?;

    ensure!(
        announced.as_ref() == Some(&expected),
        "client expiry mismatch: got {announced:?}, want {expected:?}"
    );

    info!("Clearing expiry on all online nodes");
    for node in 0..NUM_ONLINE_NODES {
        let data_dir = cli::node_data_dir(&env.data_dir, node);
        cli::node_expiry_clear(&data_dir)?;
    }

    ensure!(
        client.expiry()?.is_none(),
        "client cache should be empty after a mint-wide clear"
    );

    client.shutdown().await;

    info!("expiry: passed");
    Ok(())
}
