use anyhow::{Result, ensure};
use tracing::info;

use crate::cli;
use crate::env::{NUM_GUARDIANS, NUM_ONLINE_GUARDIANS, TestEnv, retry};

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

/// Two-phase guardian recovery test. First the two guardians taken
/// offline right after DKG come back against their stale data dirs and
/// catch up on every session ordered while they were down. Then three
/// further guardians are wiped and restored from config backups. With
/// 3-of-7 wiped, the surviving 4 can't reach threshold on their own,
/// so this exercises the bft column-state quorum gate: every wiped
/// peer must observe `threshold` peer views of its column before
/// authoring round-0, otherwise it'd fork its own column against
/// pre-wipe predecessors.
pub async fn run_test(env: &TestEnv) -> Result<()> {
    info!("bringing the offline guardians back online");
    for peer in NUM_ONLINE_GUARDIANS..NUM_GUARDIANS {
        env.restart_guardian(peer).await?;
    }

    let current = retry_session_count_at_least(env, 0, 1).await?;
    for peer in NUM_ONLINE_GUARDIANS..NUM_GUARDIANS {
        retry_session_count_at_least(env, peer, current).await?;
    }
    info!("offline guardians caught up to session {current}");

    let peers = [0_usize, 1, 2];
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
        cli::guardian_setup_restore(&data_dirs[i], &backup_paths[i])?;
    }

    let target = heights.iter().copied().max().unwrap() + 1;
    info!("waiting for guardians {peers:?} to advance to session >= {target}");
    for &peer in &peers {
        retry_session_count_at_least(env, peer, target).await?;
    }

    info!("verifying restored configs match originals");
    for (i, &peer) in peers.iter().enumerate() {
        let restored_cfg = cli::guardian_config(&data_dirs[i])?;
        ensure!(
            restored_cfg == original_cfgs[i],
            "guardian-{peer} restored config does not match original"
        );
    }

    info!("restore test OK");
    Ok(())
}
