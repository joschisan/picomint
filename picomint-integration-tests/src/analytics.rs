//! The node's analytics mirror, read through `picomint-node-cli query`
//! once the onchain, lightning and ecash suites have filled the log.

use anyhow::ensure;
use picomint_node_cli_core::NodeStatus;
use serde_json::Value;
use tracing::info;

use crate::cli;
use crate::env::TestEnv;

/// Four nodes tolerate one fault, so three votes carry a value.
const THRESHOLD: usize = 3;

pub async fn run_test(env: &TestEnv) -> anyhow::Result<()> {
    info!("analytics: test_node_query");

    let data_dir = cli::node_data_dir(&env.data_dir, 0);

    let scalar = |sql: &str| -> anyhow::Result<i64> {
        let rows = cli::node_query(&data_dir, sql)?;

        rows.first()
            .and_then(|row| row.values().next())
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow::anyhow!("query returned no integer: {sql}"))
    };

    ensure!(
        scalar("SELECT COUNT(*) FROM tx")? > 0,
        "no transactions mirrored"
    );

    ensure!(
        scalar("SELECT SUM(fee) FROM tx")? > 0,
        "transactions carry no fees"
    );

    // Outstanding notes per denomination is issuance minus spends, and a
    // note cannot be spent before it was issued.
    ensure!(
        scalar(
            "SELECT MIN(outstanding) FROM ( \
                SELECT o.denomination, o.issued - COALESCE(i.spent, 0) AS outstanding \
                FROM (SELECT denomination, COUNT(*) AS issued FROM ecash_output GROUP BY denomination) o \
                LEFT JOIN (SELECT denomination, COUNT(*) AS spent FROM ecash_input GROUP BY denomination) i \
                USING (denomination))"
        )? >= 0,
        "more notes spent than issued"
    );

    // Every settled contract was funded first, and the log orders them so.
    ensure!(
        scalar("SELECT COUNT(*) FROM lightning_input")?
            == scalar(
                "SELECT COUNT(*) FROM lightning_input i \
                 INNER JOIN ( \
                    SELECT txid, position FROM lightning_outgoing_output \
                    UNION ALL SELECT txid, position FROM lightning_incoming_output \
                 ) o ON o.txid = i.contract_txid AND o.position = i.contract_idx"
            )?,
        "a lightning input references no funded contract"
    );

    // The consensus block height is the threshold-th highest of each
    // node's latest vote — the rule the daemon applies, in SQL.
    let height = scalar(&format!(
        "SELECT height FROM (SELECT node, MAX(height) AS height FROM block_height_vote GROUP BY node) \
         ORDER BY height DESC LIMIT 1 OFFSET {}",
        THRESHOLD - 1
    ))?;

    let NodeStatus::Consensus(status) = cli::node_status(&data_dir)? else {
        anyhow::bail!("node 0 is not in consensus");
    };

    // The miner keeps producing blocks between the two calls, and the
    // mirror trails the log by a notify, so the two heights bracket.
    ensure!(
        height <= i64::from(status.block_height) && height + 5 >= i64::from(status.block_height),
        "mirror height {height} disagrees with status height {}",
        status.block_height
    );

    info!("analytics: test_node_query passed");

    Ok(())
}
