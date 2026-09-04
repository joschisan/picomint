use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use picomint_core::expiry::ExpiryStatus;
use picomint_core::invite::InviteCode;
use picomint_core::lightning::gateway::GatewayPk;
use picomint_gateway_cli_core::{
    InfoResponse, LdkChannelListResponse, LdkLightningReceiveResponse, LdkOnchainReceiveResponse,
    MintBalanceResponse, MintListResponse,
};
use picomint_node_cli_core::{InviteResponse, SetupStatus};
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
        .arg("mint")
        .arg("add")
        .arg(picomint_base32::encode(invite))
        .run_cli::<Value>()
}

pub fn gateway_mint_remove(gateway_data_dir: &Path, mint: &str) -> Result<Value> {
    gateway_cmd(gateway_data_dir)
        .arg("mint")
        .arg("remove")
        .arg(mint)
        .run_cli::<Value>()
}

pub fn gateway_mint_list(gateway_data_dir: &Path) -> Result<MintListResponse> {
    gateway_cmd(gateway_data_dir)
        .arg("mint")
        .arg("list")
        .run_cli::<MintListResponse>()
}

pub fn gateway_mint_balance(gateway_data_dir: &Path, mint: &str) -> Result<MintBalanceResponse> {
    gateway_cmd(gateway_data_dir)
        .arg("mint")
        .arg("balance")
        .arg("--id")
        .arg(mint)
        .run_cli::<MintBalanceResponse>()
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

pub fn node_setup_status(data_dir: &Path) -> Result<SetupStatus> {
    node_cmd(data_dir)
        .arg("setup")
        .arg("status")
        .run_cli::<SetupStatus>()
}

pub fn node_setup_set_local_params(
    data_dir: &Path,
    name: &str,
    mint_name: Option<&str>,
    mint_size: Option<u8>,
) -> Result<Value> {
    let mut cmd = node_cmd(data_dir);
    cmd.arg("setup").arg("set-local-params").arg(name);
    if let Some(fed_name) = mint_name {
        cmd.arg("--mint-name").arg(fed_name);
    }
    if let Some(size) = mint_size {
        cmd.arg("--mint-size").arg(size.to_string());
    }
    cmd.run_cli::<Value>()
}

pub fn node_setup_add_node(data_dir: &Path, setup_code: &str) -> Result<Value> {
    node_cmd(data_dir)
        .arg("setup")
        .arg("add-node")
        .arg(setup_code)
        .run_cli::<Value>()
}

pub fn node_setup_start_dkg(data_dir: &Path) -> Result<Value> {
    node_cmd(data_dir)
        .arg("setup")
        .arg("start-dkg")
        .run_cli::<Value>()
}

pub fn node_setup_restore(data_dir: &Path, config_path: &Path) -> Result<Value> {
    node_cmd(data_dir)
        .arg("setup")
        .arg("restore")
        .arg(config_path)
        .run_cli::<Value>()
}

pub fn node_config(data_dir: &Path) -> Result<Value> {
    node_cmd(data_dir).arg("config").run_cli::<Value>()
}

pub fn node_session_count(data_dir: &Path) -> Result<u64> {
    node_cmd(data_dir).arg("session-count").run_cli::<u64>()
}

pub fn node_lightning_gateway_add(data_dir: &Path, pk: &GatewayPk) -> Result<bool> {
    node_cmd(data_dir)
        .arg("module")
        .arg("lightning")
        .arg("gateway")
        .arg("add")
        .arg(picomint_base32::encode(pk))
        .arg("Test Gateway")
        .run_cli::<bool>()
}

pub fn node_lightning_gateway_remove(data_dir: &Path, pk: &GatewayPk) -> Result<bool> {
    node_cmd(data_dir)
        .arg("module")
        .arg("lightning")
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
