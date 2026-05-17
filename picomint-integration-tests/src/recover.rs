use anyhow::{Result, ensure};
use tracing::info;

use crate::cli;
use crate::env::{TestEnv, retry};

/// Poll until guardian `peer` reports `target` finalized sessions or
/// more. Returns the observed count.
async fn retry_session_count_at_least(env: &TestEnv, peer: usize, target: u64) -> Result<u64> {
    let data_dir = env.data_dir.join(format!("guardian-{peer}"));

    retry(
        &format!("guardian-{peer} session count >= {target}"),
        || {
            let data_dir = data_dir.clone();
            async move {
                let count = cli::guardian_session_count(&data_dir)?;
                ensure!(count >= target, "session count {count} < {target}");
                Ok(count)
            }
        },
    )
    .await
}

/// Wipe two guardians at once, recover both, and verify the federation
/// resumes ordering sessions past where it was. With 2-of-4 wiped, the
/// surviving 2 can't reach threshold on their own, so this exercises
/// the bft column-state quorum gate: both wiped peers must observe
/// `threshold` peer views of their column before authoring round-0,
/// otherwise they'd fork their own column against pre-wipe predecessors.
pub async fn run_test(env: &TestEnv) -> Result<()> {
    let peers = [0_usize, 1];
    let data_dirs: Vec<_> = peers
        .iter()
        .map(|p| env.data_dir.join(format!("guardian-{p}")))
        .collect();

    info!("waiting for guardians {peers:?} to finalize a session");
    let mut heights = Vec::with_capacity(peers.len());
    for &peer in &peers {
        heights.push(retry_session_count_at_least(env, peer, 1).await?);
    }
    info!(
        "recorded session counts: {:?}",
        peers.iter().zip(&heights).collect::<Vec<_>>()
    );

    info!("backing up configs");
    let mut original_cfgs = Vec::with_capacity(peers.len());
    let mut backup_paths = Vec::with_capacity(peers.len());
    for (i, &peer) in peers.iter().enumerate() {
        let cfg = cli::guardian_config(&data_dirs[i])?;
        let backup_path = env.data_dir.join(format!("config-{peer}.json"));
        std::fs::write(&backup_path, serde_json::to_vec_pretty(&cfg)?)?;
        original_cfgs.push(cfg);
        backup_paths.push(backup_path);
    }

    info!("killing guardians {peers:?} and wiping their data dirs");
    for &peer in &peers {
        env.wipe_guardian(peer).await?;
    }

    info!("restarting guardians {peers:?} (fresh)");
    for &peer in &peers {
        env.restart_guardian(peer).await?;
    }

    for (i, &peer) in peers.iter().enumerate() {
        let data_dir = data_dirs[i].clone();
        retry(&format!("guardian-{peer} in setup mode"), || {
            let data_dir = data_dir.clone();
            async move { cli::guardian_setup_status(&data_dir) }
        })
        .await?;
    }

    info!("uploading saved configs");
    for (i, &peer) in peers.iter().enumerate() {
        info!("uploading config for guardian-{peer}");
        cli::guardian_setup_recover(&data_dirs[i], &backup_paths[i])?;
    }

    let target = heights.iter().copied().max().unwrap() + 1;
    info!("waiting for guardians {peers:?} to advance to session >= {target}");
    for &peer in &peers {
        retry_session_count_at_least(env, peer, target).await?;
    }

    info!("verifying recovered configs match originals");
    for (i, &peer) in peers.iter().enumerate() {
        let recovered_cfg = cli::guardian_config(&data_dirs[i])?;
        ensure!(
            recovered_cfg == original_cfgs[i],
            "guardian-{peer} recovered config does not match original"
        );
    }

    info!("recover test OK");
    Ok(())
}
