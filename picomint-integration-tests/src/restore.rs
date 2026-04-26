use std::sync::Arc;

use anyhow::{Result, ensure};
use picomint_client::Client;
use serde_json::Value;
use tracing::info;

use crate::cli;
use crate::env::{TestEnv, retry};

/// Peers we kill and wipe. With NUM_GUARDIANS = 4 and threshold = 3,
/// wiping 3 leaves only one peer alive — well below threshold. The
/// federation halts until at least two of the wiped peers come back
/// online (via [`RESTORED_PEERS`]).
const WIPED_PEERS: [usize; 3] = [1, 2, 3];

/// Peers we restore from backup. The remaining wiped peer (3) stays
/// dead — exercising the case where the federation must continue
/// indefinitely without one of its members ever returning.
const RESTORED_PEERS: [usize; 2] = [1, 2];

/// Poll until guardian `peer_idx`'s finalized session count exceeds `floor`.
async fn retry_session_count_above(env: &TestEnv, peer_idx: usize, floor: u64) -> Result<u64> {
    let data_dir = env.data_dir.join(format!("server-{peer_idx}"));
    retry(
        &format!("server-{peer_idx} session count > {floor}"),
        || {
            let data_dir = data_dir.clone();
            async move {
                let count = cli::server_session_count(&data_dir)?;
                ensure!(count > floor, "session count still {count}");
                Ok(count)
            }
        },
    )
    .await
}

pub async fn run_test(env: &TestEnv, client: &Arc<Client>) -> Result<()> {
    info!("waiting for federation to finalize a session");
    let pre_wipe_count = retry_session_count_above(env, WIPED_PEERS[0], 0).await?;
    info!("pre-wipe session count = {pre_wipe_count}");

    info!("backing up configs of peers {:?}", RESTORED_PEERS);
    let mut backups: Vec<(usize, Value, std::path::PathBuf)> = Vec::new();
    for &peer_idx in &RESTORED_PEERS {
        let data_dir = env.data_dir.join(format!("server-{peer_idx}"));
        let cfg = cli::server_config(&data_dir)?;
        let backup_path = env.data_dir.join(format!("config-{peer_idx}.json"));
        std::fs::write(&backup_path, serde_json::to_vec_pretty(&cfg)?)?;
        backups.push((peer_idx, cfg, backup_path));
    }

    info!("killing and wiping peers {:?}", WIPED_PEERS);
    for &peer_idx in &WIPED_PEERS {
        env.wipe_guardian(peer_idx).await?;
    }

    info!("restarting peers {:?} (fresh data dirs)", RESTORED_PEERS);
    for &peer_idx in &RESTORED_PEERS {
        env.restart_guardian(peer_idx).await?;
    }

    info!("waiting for restored peers to enter setup mode");
    for &peer_idx in &RESTORED_PEERS {
        let data_dir = env.data_dir.join(format!("server-{peer_idx}"));
        retry(&format!("server-{peer_idx} in setup mode"), || async {
            cli::server_setup_status(&data_dir)
        })
        .await?;
    }

    info!("uploading saved configs");
    for (peer_idx, _, backup_path) in &backups {
        let data_dir = env.data_dir.join(format!("server-{peer_idx}"));
        cli::server_setup_restore(&data_dir, backup_path)?;
    }

    info!("waiting for federation to advance past pre-wipe session count");
    for &peer_idx in &RESTORED_PEERS {
        retry_session_count_above(env, peer_idx, pre_wipe_count).await?;
    }

    info!("verifying restored configs match originals");
    for (peer_idx, original_cfg, _) in &backups {
        let data_dir = env.data_dir.join(format!("server-{peer_idx}"));
        let restored_cfg = cli::server_config(&data_dir)?;
        ensure!(
            &restored_cfg == original_cfg,
            "server-{peer_idx} restored config does not match original"
        );
    }

    // The session-count check above proves consensus is making progress, but
    // doesn't prove every peer's iroh public-API endpoint is reachable from
    // this client. Subsequent tests fire client requests immediately, so wait
    // here until a threshold-quorum liveness probe succeeds.
    info!("waiting for federation liveness via client API");
    retry("federation liveness", || async {
        client
            .api()
            .liveness()
            .await
            .map_err(|e| anyhow::anyhow!("liveness failed: {e:?}"))
    })
    .await?;

    info!("restore test OK");
    Ok(())
}
