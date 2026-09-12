//! Decommissioning: a threshold of nodes export their sweep secrets and
//! `picomint-sweep` drains the mint wallet to a bitcoind address.

use anyhow::{Context, ensure};
use bitcoincore_rpc::RpcApi;
use picomint_core::NumNodes;
use picomint_node_cli_core::NodeStatus;
use tokio::task::block_in_place;
use tracing::info;

use crate::cli;
use crate::env::{NUM_NODES, TestEnv, bitcoind_url, retry};

pub async fn run_test(env: &TestEnv) -> anyhow::Result<()> {
    let threshold = NumNodes::from(NUM_NODES).threshold();

    let data_dirs: Vec<_> = (0..threshold)
        .map(|node| cli::node_data_dir(&env.data_dir, node))
        .collect();

    // A sweep secret is tweaked for the mint UTXO its node currently
    // holds, and the tool only finds a confirmed one. The restored nodes
    // are still catching up when the restore test ends, so wait until
    // every exporting node holds the same transaction tip and nothing is
    // pending, mining a block per attempt since confirmations are the work.
    info!("waiting for {threshold} nodes to agree on a confirmed mint utxo");
    retry("nodes agree on the mint utxo", || {
        let data_dirs = data_dirs.clone();
        async move {
            env.mine_blocks(1);

            let tips = data_dirs
                .iter()
                .map(|data_dir| match cli::node_status(data_dir)? {
                    NodeStatus::Consensus(phase) => {
                        ensure!(phase.pending_txs.is_empty(), "mint txs still pending");
                        Ok(phase.tx_tip)
                    }
                    phase => anyhow::bail!("node is not in consensus: {phase:?}"),
                })
                .collect::<anyhow::Result<Vec<_>>>()?;

            ensure!(
                tips[0].is_some() && tips.iter().all(|tip| *tip == tips[0]),
                "nodes disagree on the mint transaction tip: {tips:?}"
            );

            Ok(())
        }
    })
    .await?;

    info!("exporting sweep secrets from {threshold} nodes");
    let secrets = data_dirs
        .iter()
        .map(|data_dir| cli::node_onchain_sweep(data_dir).map(|response| response.secret))
        .collect::<anyhow::Result<Vec<_>>>()?;

    let destination = block_in_place(|| env.bitcoind.get_new_address(None, None))?
        .require_network(bitcoin::Network::Regtest)?;

    info!("sweeping the mint wallet to {destination}");
    let report = cli::sweep(NUM_NODES, &destination, &bitcoind_url(), &secrets)?;

    let value = report["value_sat"]
        .as_u64()
        .context("sweep report carries no value")?;

    env.mine_blocks(1);

    let received = block_in_place(|| env.bitcoind.get_received_by_address(&destination, Some(1)))?;

    ensure!(
        received.to_sat() == value,
        "swept {value} sat but {destination} received {received}"
    );

    info!("sweep OK: {value} sat in {}", report["txid"]);
    Ok(())
}
