use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use picomint_core::expiry::ExpiryStatus;
use picomint_core::invite::InviteCode;
use picomint_core::lightning::gateway::GatewayPk;
use picomint_gateway_cli_core::{
    ClientBalanceResponse, ClientListResponse, InfoResponse, LdkChannelListResponse,
    LdkLightningReceiveResponse, LdkOnchainReceiveResponse,
};
use picomint_node_cli_core::{
    InviteResponse, NodeStatus, OnchainStatusResponse, PendingTxsResponse, SweepResponse,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

trait RunCli {
    fn run_cli<T: DeserializeOwned>(&mut self) -> Result<T>;
}

impl RunCli for Command {
    fn run_cli<T: DeserializeOwned>(&mut self) -> Result<T> {
        let output = self.output().context("Failed to run CLI")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            bail!("CLI failed:\nstdout: {stdout}\nstderr: {stderr}");
        }

        let stdout = String::from_utf8(output.stdout)?;
        serde_json::from_str(stdout.trim()).context(format!("Failed to parse CLI output: {stdout}"))
    }
}

fn gateway_cmd(gateway_data_dir: &Path) -> Command {
    let mut cmd = Command::new("target/release/picomint-gateway-cli");
    cmd.arg("--data-dir").arg(gateway_data_dir);
    cmd
}

fn node_cmd(data_dir: &Path) -> Command {
    let mut cmd = Command::new("target/release/picomint-node-cli");
    cmd.arg("--data-dir").arg(data_dir);
    cmd
}

/// Helper to compute a node's data directory from the shared test
/// temp root, mirroring `env::start_node`'s layout.
pub fn node_data_dir(base: &Path, node: usize) -> PathBuf {
    base.join(format!("node-{node}"))
}

// ── Gateway CLI wrappers ────────────────────────────────────────────────────

pub fn gateway_info(gateway_data_dir: &Path) -> Result<InfoResponse> {
    gateway_cmd(gateway_data_dir)
        .arg("info")
        .run_cli::<InfoResponse>()
}

pub fn gateway_mint_add(gateway_data_dir: &Path, invite: &InviteCode) -> Result<Value> {
    gateway_cmd(gateway_data_dir)
        .arg("client")
        .arg("add")
        .arg(picomint_base32::encode(invite))
        .run_cli::<Value>()
}

pub fn gateway_mint_remove(gateway_data_dir: &Path, mint: &str) -> Result<Value> {
    gateway_cmd(gateway_data_dir)
        .arg("client")
        .arg("remove")
        .arg(mint)
        .run_cli::<Value>()
}

pub fn gateway_mint_list(gateway_data_dir: &Path) -> Result<ClientListResponse> {
    gateway_cmd(gateway_data_dir)
        .arg("client")
        .arg("list")
        .run_cli::<ClientListResponse>()
}

pub fn gateway_mint_balance(gateway_data_dir: &Path, mint: &str) -> Result<ClientBalanceResponse> {
    gateway_cmd(gateway_data_dir)
        .arg("client")
        .arg("balance")
        .arg(mint)
        .arg("primary")
        .run_cli::<ClientBalanceResponse>()
}

pub fn gateway_ldk_onchain_receive(gateway_data_dir: &Path) -> Result<LdkOnchainReceiveResponse> {
    gateway_cmd(gateway_data_dir)
        .arg("ldk")
        .arg("onchain")
        .arg("receive")
        .run_cli::<LdkOnchainReceiveResponse>()
}

pub fn gateway_ldk_channel_open(
    gateway_data_dir: &Path,
    node_id: &str,
    lightning_addr: &str,
    channel_sat: u64,
    push_sat: u64,
) -> Result<Value> {
    gateway_cmd(gateway_data_dir)
        .arg("ldk")
        .arg("channel")
        .arg("open")
        .arg(node_id)
        .arg(lightning_addr)
        .arg(channel_sat.to_string())
        .arg("--push-amount-sat")
        .arg(push_sat.to_string())
        .run_cli::<Value>()
}

pub fn gateway_ldk_channel_list(gateway_data_dir: &Path) -> Result<LdkChannelListResponse> {
    gateway_cmd(gateway_data_dir)
        .arg("ldk")
        .arg("channel")
        .arg("list")
        .run_cli::<LdkChannelListResponse>()
}

pub fn gateway_ldk_lightning_receive(
    gateway_data_dir: &Path,
    amount_msat: u64,
) -> Result<LdkLightningReceiveResponse> {
    gateway_cmd(gateway_data_dir)
        .arg("ldk")
        .arg("lightning")
        .arg("receive")
        .arg(amount_msat.to_string())
        .run_cli::<LdkLightningReceiveResponse>()
}

pub fn gateway_ldk_lightning_send(gateway_data_dir: &Path, invoice: &str) -> Result<Value> {
    gateway_cmd(gateway_data_dir)
        .arg("ldk")
        .arg("lightning")
        .arg("send")
        .arg(invoice)
        .run_cli::<Value>()
}

// ── Node CLI wrappers ───────────────────────────────────────────────────

pub fn node_invite(data_dir: &Path) -> Result<InviteResponse> {
    node_cmd(data_dir).arg("invite").run_cli::<InviteResponse>()
}

pub fn node_status(data_dir: &Path) -> Result<NodeStatus> {
    node_cmd(data_dir).arg("status").run_cli::<NodeStatus>()
}

pub fn node_setup_init(
    data_dir: &Path,
    name: &str,
    mint_name: Option<&str>,
    mint_size: Option<u8>,
) -> Result<Value> {
    let mut cmd = node_cmd(data_dir);
    cmd.arg("setup").arg("init").arg(name);
    if let Some(fed_name) = mint_name {
        cmd.arg("--mint-name").arg(fed_name);
    }
    if let Some(size) = mint_size {
        cmd.arg("--mint-size").arg(size.to_string());
    }
    cmd.run_cli::<Value>()
}

pub fn node_setup_add(data_dir: &Path, setup_code: &str) -> Result<Value> {
    node_cmd(data_dir)
        .arg("setup")
        .arg("add")
        .arg(setup_code)
        .run_cli::<Value>()
}

pub fn node_setup_confirm(data_dir: &Path) -> Result<Value> {
    node_cmd(data_dir)
        .arg("setup")
        .arg("confirm")
        .run_cli::<Value>()
}

pub fn node_setup_restore(data_dir: &Path, backup_path: &Path) -> Result<Value> {
    node_cmd(data_dir)
        .arg("setup")
        .arg("restore")
        .stdin(std::fs::File::open(backup_path)?)
        .run_cli::<Value>()
}

pub fn node_backup(data_dir: &Path) -> Result<Value> {
    node_cmd(data_dir).arg("backup").run_cli::<Value>()
}

pub fn node_session_count(data_dir: &Path) -> Result<u64> {
    match node_status(data_dir)? {
        NodeStatus::Consensus(phase) => Ok(u64::from(phase.session_count)),
        status => bail!("node is not in consensus: {status:?}"),
    }
}

pub fn node_onchain_pending_txs(data_dir: &Path) -> Result<PendingTxsResponse> {
    node_cmd(data_dir)
        .arg("onchain")
        .arg("pending-txs")
        .run_cli::<PendingTxsResponse>()
}

pub fn node_onchain_status(data_dir: &Path) -> Result<OnchainStatusResponse> {
    node_cmd(data_dir)
        .arg("onchain")
        .arg("status")
        .run_cli::<OnchainStatusResponse>()
}

pub fn node_onchain_sweep(data_dir: &Path) -> Result<SweepResponse> {
    node_cmd(data_dir)
        .arg("onchain")
        .arg("sweep")
        .run_cli::<SweepResponse>()
}

/// Runs `picomint-sweep` against the test bitcoind with the given secrets
/// and returns its report. The fee rate is explicit because a regtest
/// bitcoind never has an estimate.
pub fn sweep(
    nodes: usize,
    destination: &bitcoin::Address,
    bitcoind_url: &str,
    secrets: &[String],
) -> Result<Value> {
    let mut cmd = Command::new("target/release/picomint-sweep");
    cmd.arg(nodes.to_string())
        .arg(destination.to_string())
        .arg("--bitcoind-url")
        .arg(bitcoind_url)
        .arg("--fee-rate-sat-per-vb")
        .arg("2");
    for secret in secrets {
        cmd.arg("--secret").arg(secret);
    }
    cmd.run_cli::<Value>()
}

pub fn node_lightning_gateway_add(data_dir: &Path, pk: &GatewayPk) -> Result<bool> {
    node_cmd(data_dir)
        .arg("gateway")
        .arg("add")
        .arg(picomint_base32::encode(pk))
        .arg("Test Gateway")
        .run_cli::<bool>()
}

pub fn node_lightning_gateway_remove(data_dir: &Path, pk: &GatewayPk) -> Result<bool> {
    node_cmd(data_dir)
        .arg("gateway")
        .arg("remove")
        .arg(picomint_base32::encode(pk))
        .run_cli::<bool>()
}

pub fn node_expiry_set(
    data_dir: &Path,
    timestamp: u64,
    successor: Option<&InviteCode>,
) -> Result<Value> {
    let mut cmd = node_cmd(data_dir);
    cmd.arg("expiry")
        .arg("set")
        .arg("--timestamp")
        .arg(timestamp.to_string());
    if let Some(invite) = successor {
        cmd.arg("--successor").arg(picomint_base32::encode(invite));
    }
    cmd.run_cli::<Value>()
}

pub fn node_expiry_clear(data_dir: &Path) -> Result<Value> {
    node_cmd(data_dir)
        .arg("expiry")
        .arg("clear")
        .run_cli::<Value>()
}

pub fn node_expiry_status(data_dir: &Path) -> Result<Option<ExpiryStatus>> {
    node_cmd(data_dir)
        .arg("expiry")
        .arg("status")
        .run_cli::<Option<ExpiryStatus>>()
}
