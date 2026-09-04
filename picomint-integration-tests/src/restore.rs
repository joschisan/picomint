use anyhow::{Result, ensure};
use tracing::info;

use crate::cli;
use crate::env::{NUM_NODES, NUM_ONLINE_NODES, TestEnv, retry};

/// Poll until node `node` reports `target` finalized sessions or
/// more. Returns the observed count.
async fn retry_session_count_at_least(env: &TestEnv, node: usize, target: u64) -> Result<u64> {
    let data_dir = env.data_dir.join(format!("node-{node}"));

    retry(&format!("node-{node} session count >= {target}"), || {
        let data_dir = data_dir.clone();
        async move {
            let count = cli::node_session_count(&data_dir)?;
            ensure!(count >= target, "session count {count} < {target}");
            Ok(count)
        }
    })
    .await
}

/// Two-phase node recovery test. First the two nodes taken
/// offline right after DKG come back against their stale data dirs and
/// catch up on every session ordered while they were down. Then three
/// further nodes are wiped and restored from config backups. With
/// 3-of-7 wiped, the surviving 4 can't reach threshold on their own,
/// so this exercises the bft column-state quorum gate: every wiped
/// node must observe `threshold` node views of its column before
/// authoring round-0, otherwise it'd fork its own column against
/// pre-wipe predecessors.
pub async fn run_test(env: &TestEnv) -> Result<()> {
    info!("bringing the offline nodes back online");
    for node in NUM_ONLINE_NODES..NUM_NODES {
        env.restart_node(node).await?;
    }

    let current = retry_session_count_at_least(env, 0, 1).await?;
    for node in NUM_ONLINE_NODES..NUM_NODES {
        retry_session_count_at_least(env, node, current).await?;
    }
    info!("offline nodes caught up to session {current}");

    let nodes = [0_usize, 1, 2];
    let data_dirs: Vec<_> = nodes
        .iter()
        .map(|p| env.data_dir.join(format!("node-{p}")))
        .collect();

    info!("waiting for nodes {nodes:?} to finalize a session");
    let mut heights = Vec::with_capacity(nodes.len());
    for &node in &nodes {
        heights.push(retry_session_count_at_least(env, node, 1).await?);
    }
    info!(
        "recorded session counts: {:?}",
        nodes.iter().zip(&heights).collect::<Vec<_>>()
    );

    info!("backing up configs");
    let mut original_cfgs = Vec::with_capacity(nodes.len());
    let mut backup_paths = Vec::with_capacity(nodes.len());
    for (i, &node) in nodes.iter().enumerate() {
        let cfg = cli::node_config(&data_dirs[i])?;
        let backup_path = env.data_dir.join(format!("config-{node}.json"));
        std::fs::write(&backup_path, serde_json::to_vec_pretty(&cfg)?)?;
        original_cfgs.push(cfg);
        backup_paths.push(backup_path);
    }

    info!("killing nodes {nodes:?} and wiping their data dirs");
    for &node in &nodes {
        env.wipe_node(node).await?;
    }

    info!("restarting nodes {nodes:?} (fresh)");
    for &node in &nodes {
        env.restart_node(node).await?;
    }

    for (i, &node) in nodes.iter().enumerate() {
        let data_dir = data_dirs[i].clone();
        retry(&format!("node-{node} in setup mode"), || {
            let data_dir = data_dir.clone();
            async move { cli::node_setup_status(&data_dir) }
        })
        .await?;
    }

    info!("uploading saved configs");
    for (i, &node) in nodes.iter().enumerate() {
        info!("uploading config for node-{node}");
        cli::node_setup_restore(&data_dirs[i], &backup_paths[i])?;
    }

    let target = heights.iter().copied().max().unwrap() + 1;
    info!("waiting for nodes {nodes:?} to advance to session >= {target}");
    for &node in &nodes {
        retry_session_count_at_least(env, node, target).await?;
    }

    info!("verifying restored configs match originals");
    for (i, &node) in nodes.iter().enumerate() {
        let restored_cfg = cli::node_config(&data_dirs[i])?;
        ensure!(
            restored_cfg == original_cfgs[i],
            "node-{node} restored config does not match original"
        );
    }

    info!("restore test OK");
    Ok(())
}
