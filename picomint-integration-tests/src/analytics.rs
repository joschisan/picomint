//! The node's analytics mirror, read through `picomint-node-cli query`
//! once the onchain, lightning and ecash suites have filled the log.

use anyhow::ensure;
use picomint_node_cli_core::NodeStatus;
use serde_json::Value;
use tracing::info;

use crate::cli;
use crate::env::TestEnv;

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
        scalar("SELECT COUNT(*) FROM ecash_output")? > 0,
        "no notes mirrored"
    );

    // Outstanding notes per denomination is issuance minus spends, and a
    // note cannot be spent before it was issued.
    ensure!(
        scalar(
            "SELECT MIN(outstanding) FROM ( \
                SELECT denomination, SUM(delta) AS outstanding \
                FROM (SELECT denomination, 1 AS delta FROM ecash_output \
                      UNION ALL SELECT denomination, -1 FROM ecash_input) \
                GROUP BY denomination)"
        )? >= 0,
        "more notes spent than issued"
    );

    // Every claimed deposit names a tracked output of the same value.
    ensure!(
        scalar("SELECT COUNT(*) FROM onchain_input")? > 0,
        "no deposits mirrored"
    );

    ensure!(
        scalar("SELECT COUNT(*) FROM onchain_input")?
            == scalar(
                "SELECT COUNT(*) FROM onchain_input d \
                 INNER JOIN onchain_tracked_output o USING (output_index) \
                 WHERE o.value_sat = d.value_sat AND o.btc_txid = d.btc_txid AND o.vout = d.vout"
            )?,
        "a deposit claims no tracked output of its value"
    );

    // Custody is the latest wallet UTXO; it covers the ecash in circulation.
    ensure!(
        scalar(
            "SELECT (SELECT output_sat FROM onchain_tx ORDER BY tx_index DESC LIMIT 1) * 1000 \
                  - (SELECT SUM(delta * (1 << denomination)) \
                     FROM (SELECT denomination, 1 AS delta FROM ecash_output \
                           UNION ALL SELECT denomination, -1 FROM ecash_input))"
        )? >= 0,
        "custody does not cover the ecash in circulation"
    );

    // Every settled contract was funded first, and the log orders them so.
    ensure!(
        scalar(
            "SELECT COUNT(*) FROM ( \
                SELECT id, contract FROM lightning_input_outgoing_claim \
                UNION ALL SELECT id, contract FROM lightning_input_outgoing_refund \
                UNION ALL SELECT id, contract FROM lightning_input_outgoing_cancel) s \
             LEFT JOIN lightning_output_outgoing c ON c.outpoint = s.contract \
             WHERE c.outpoint IS NULL OR c.id > s.id"
        )? == 0,
        "an outgoing settlement references no funded contract"
    );

    ensure!(
        scalar(
            "SELECT COUNT(*) FROM ( \
                SELECT id, contract FROM lightning_input_incoming_claim \
                UNION ALL SELECT id, contract FROM lightning_input_incoming_refund) s \
             LEFT JOIN lightning_output_incoming c ON c.outpoint = s.contract \
             WHERE c.outpoint IS NULL OR c.id > s.id"
        )? == 0,
        "an incoming settlement references no funded contract"
    );

    let height = scalar("SELECT height FROM height ORDER BY id DESC LIMIT 1")?;

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
