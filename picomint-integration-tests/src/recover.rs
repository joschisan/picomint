use anyhow::{Result, ensure};
use tracing::info;

use crate::cli;
use crate::env::{TestEnv, retry};

/// Poll until guardian `peer` reports a non-zero finalized session
/// count — proves it's participating in consensus.
async fn retry_non_zero_session_count(env: &TestEnv, peer: usize) -> Result<u64> {
    let data_dir = env.data_dir.join(format!("guardian-{peer}"));

    retry(&format!("guardian-{peer} session count > 0"), || {
        let data_dir = data_dir.clone();
        async move {
            let count = cli::guardian_session_count(&data_dir)?;
            ensure!(count > 0, "session count still 0");
            Ok(count)
        }
    })
    .await
}

pub async fn run_test(env: &TestEnv) -> Result<()> {
    let peer = 0;
    let data_dir = env.data_dir.join(format!("guardian-{peer}"));

    info!("waiting for guardian-{peer} to finalize a session");
    retry_non_zero_session_count(env, peer).await?;

    info!("backing up config");
    let original_cfg = cli::guardian_config(&data_dir)?;
    let backup_path = env.data_dir.join("config.json");
    std::fs::write(&backup_path, serde_json::to_vec_pretty(&original_cfg)?)?;

    info!("killing guardian-{peer} and wiping its data dir");
    env.wipe_guardian(peer).await?;

    info!("restarting guardian-{peer} (fresh)");
    env.restart_guardian(peer).await?;

    retry(&format!("guardian-{peer} in setup mode"), || async {
        cli::guardian_setup_status(&data_dir)
    })
    .await?;

    info!("uploading saved config");
    cli::guardian_setup_recover(&data_dir, &backup_path)?;

    info!("waiting for guardian-{peer} to rejoin consensus");
    retry_non_zero_session_count(env, peer).await?;

    info!("verifying recovered config matches original");
    let recovered_cfg = cli::guardian_config(&data_dir)?;
    ensure!(
        recovered_cfg == original_cfg,
        "recovered config does not match original"
    );

    info!("recover test OK");
    Ok(())
}
