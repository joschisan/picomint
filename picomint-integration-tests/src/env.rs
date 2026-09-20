use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, ensure};
use bitcoin::Network;
use bitcoincore_rpc::RpcApi;
use picomint_core::invite::InviteCode;
use picomint_core::lightning::gateway::GatewayPk;
use picomint_core::swap::broker::BrokerPk;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::task::block_in_place;
use tracing::info;

use crate::cli;
use crate::client::TestClient;

pub const BTC_RPC_PORT: u16 = 18443;
pub const NODE_BASE_PORT: u16 = 17000;
pub const PORTS_PER_NODE: u16 = 5;
pub const NUM_NODES: usize = 7;

/// Nodes `NUM_ONLINE_NODES..` are taken offline right after DKG,
/// so the entire suite runs against a mint at exactly quorum
/// (5 of 7). The restore test brings them back online.
pub const NUM_ONLINE_NODES: usize = 5;
pub const GW_PORT: u16 = 28175;
pub const BROKER_PORT: u16 = 28177;
pub const GW_LN_PORT: u16 = 9735;
pub const TEST_LDK_PORT: u16 = 9736;
pub const LNURL_DAEMON_PORT: u16 = 28176;

const BTC_RPC_USER: &str = "bitcoin";
const BTC_RPC_PASS: &str = "bitcoin";

/// The `BITCOIND_URL` every daemon under test is pointed at.
pub fn bitcoind_url() -> String {
    format!("http://{BTC_RPC_USER}:{BTC_RPC_PASS}@127.0.0.1:{BTC_RPC_PORT}")
}

fn dummy_address() -> bitcoin::Address {
    "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080"
        .parse::<bitcoin::Address<bitcoin::address::NetworkUnchecked>>()
        .expect("valid address")
        .require_network(bitcoin::Network::Regtest)
        .expect("regtest address")
}

pub struct TestEnv {
    pub ldk_node: Arc<ldk_node::Node>,
    pub data_dir: std::path::PathBuf,
    pub bitcoind: bitcoincore_rpc::Client,
    pub invite: InviteCode,
    pub gateway_data_dir: std::path::PathBuf,
    pub gateway_pk: GatewayPk,
    pub broker_data_dir: std::path::PathBuf,
    pub broker_pk: BrokerPk,
    pub lnurl_daemon_url: String,
    pub client_counter: AtomicU64,
    /// One per node, indexed by node id. `None` once we've killed it.
    pub node_processes: Mutex<Vec<Option<Child>>>,
}

impl TestEnv {
    pub fn setup(runtime: Arc<tokio::runtime::Runtime>) -> anyhow::Result<(Self, TestClient)> {
        let data_dir = tempfile::TempDir::new()?.keep();
        let base = data_dir.as_path();
        info!("Test data directory: {}", base.display());

        let bitcoind = Self::connect_bitcoind(&runtime)?;

        // Fund bitcoind's own wallet so peg-ins can be regular (non-coinbase)
        // transactions — avoids the 100-block coinbase maturity wait.
        let funding_addr = bitcoind
            .get_new_address(None, None)?
            .require_network(bitcoin::Network::Regtest)?;
        bitcoind.generate_to_address(101, &funding_addr)?;

        Self::spawn_miner_thread()?;

        let mut node_processes = Vec::with_capacity(NUM_NODES);
        for i in 0..NUM_NODES {
            let child = runtime.block_on(start_node(base, i))?;
            node_processes.push(Some(child));
        }

        info!("Running DKG...");
        let node_data_dirs: Vec<_> = (0..NUM_NODES)
            .map(|i| base.join(format!("node-{i}")))
            .collect();
        runtime.block_on(run_dkg(&node_data_dirs, "Test Mint"))?;

        let node0_data_dir = node_data_dirs[0].clone();
        let invite = runtime
            .block_on(retry("fetch invite code", || async {
                cli::node_invite(&node0_data_dir)
            }))?
            .invite;
        info!("Mint ready");

        // Take the last two nodes offline so the rest of the suite
        // runs against a mint at exactly quorum. Wait for each to have
        // written its config first — that is its DKG output persisted,
        // so it can come back from its data dir; bft state is committed
        // unit by unit and replays from wherever it was cut off.
        for node in NUM_ONLINE_NODES..NUM_NODES {
            let data_dir = &node_data_dirs[node];
            runtime.block_on(retry(&format!("node-{node} wrote its config"), || async {
                cli::node_backup(data_dir).map(|_| ())
            }))?;

            let mut child = node_processes[node].take().expect("node was started");
            runtime.block_on(async {
                child.kill().await?;
                child.wait().await?;
                anyhow::Ok(())
            })?;
            info!("Stopped node-{node}");
        }

        let client_counter = AtomicU64::new(0);
        let client_send = runtime.block_on(start_client(
            base,
            client_counter.fetch_add(1, Ordering::Relaxed),
            &invite,
        ))?;

        runtime.block_on(start_gateway(base, "gateway", GW_PORT, GW_LN_PORT))?;

        let gateway_data_dir = base.join("gateway");

        info!("Waiting for gateway...");
        let gateway_pk = runtime.block_on(retry("gateway ready", || async {
            Ok(cli::gateway_info(&gateway_data_dir)?.gateway_pk)
        }))?;
        info!(
            "Gateway ready, gateway_pk={}",
            picomint_base32::encode(&gateway_pk)
        );

        runtime.block_on(start_lnurl_daemon(base, LNURL_DAEMON_PORT))?;
        let lnurl_daemon_url = format!("http://127.0.0.1:{LNURL_DAEMON_PORT}/");
        info!("LNURL daemon started on {LNURL_DAEMON_PORT}");

        info!("Connecting gateway to mint...");
        cli::gateway_mint_add(&gateway_data_dir, &invite)?;
        info!("Gateway connected");

        runtime.block_on(start_broker(base, "broker", BROKER_PORT))?;

        let broker_data_dir = base.join("broker");

        info!("Waiting for broker...");
        let broker_pk = runtime.block_on(retry("broker ready", || async {
            Ok(cli::broker_info(&broker_data_dir)?.broker_pk)
        }))?;
        info!(
            "Broker ready, broker_pk={}",
            picomint_base32::encode(&broker_pk)
        );

        info!("Connecting broker to the mint...");
        cli::broker_mint_add(&broker_data_dir, &invite)?;

        // The broker funds every swap out of its own balance in the mint,
        // so it gets one deposit before any test runs. Every swap in the
        // suite goes back into the mint it left, which exercises the
        // whole broker path on one mint.
        info!("Funding the broker...");
        runtime.block_on(fund_broker(&bitcoind, &broker_data_dir, invite.mint))?;
        info!("Broker funded");

        info!("Building freestanding LDK node...");
        let ldk_node = build_ldk_node(base, runtime.clone())?;
        info!("LDK node built: {}", ldk_node.node_id());

        info!("Funding gateway and opening channel to LDK node...");
        runtime.block_on(open_channel(&bitcoind, &gateway_data_dir, &ldk_node))?;
        info!("Channel opened");

        Ok((
            Self {
                ldk_node,
                data_dir,
                bitcoind,
                invite,
                gateway_data_dir,
                gateway_pk,
                broker_data_dir,
                broker_pk,
                lnurl_daemon_url,
                client_counter,
                node_processes: Mutex::new(node_processes),
            },
            client_send,
        ))
    }

    /// SIGKILL a single node process and delete its data directory,
    /// simulating a total disk loss. Use `restart_node` to bring it
    /// back up against an empty data dir.
    pub async fn wipe_node(&self, node: usize) -> anyhow::Result<()> {
        let mut procs = self.node_processes.lock().await;
        if let Some(mut child) = procs[node].take() {
            child.kill().await?;
            child.wait().await?;
        }
        drop(procs);

        let data_dir = self.data_dir.join(format!("node-{node}"));
        tokio::fs::remove_dir_all(&data_dir).await?;
        Ok(())
    }

    /// Spawn a fresh daemon for `node` against its existing data dir.
    pub async fn restart_node(&self, node: usize) -> anyhow::Result<()> {
        let child = start_node(&self.data_dir, node).await?;
        self.node_processes.lock().await[node] = Some(child);
        Ok(())
    }

    /// Mine one regtest block per second for the lifetime of the test.
    /// Nodes only propose block-height votes when the height changes,
    /// so without steadily arriving blocks an idle mint orders
    /// nothing. Faster than this and the lightning suite's CLTV margins
    /// no longer hold; the restore suite mines its own blocks while it
    /// waits for sessions.
    fn spawn_miner_thread() -> anyhow::Result<()> {
        let url = format!("http://127.0.0.1:{BTC_RPC_PORT}/wallet/default");
        let auth =
            bitcoincore_rpc::Auth::UserPass(BTC_RPC_USER.to_string(), BTC_RPC_PASS.to_string());
        let client = bitcoincore_rpc::Client::new(&url, auth)?;

        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                client.generate_to_address(1, &dummy_address()).ok();
            }
        });

        Ok(())
    }

    fn connect_bitcoind(
        runtime: &tokio::runtime::Runtime,
    ) -> anyhow::Result<bitcoincore_rpc::Client> {
        let url = format!("http://127.0.0.1:{BTC_RPC_PORT}/wallet/default");
        let auth =
            bitcoincore_rpc::Auth::UserPass(BTC_RPC_USER.to_string(), BTC_RPC_PASS.to_string());
        let client = bitcoincore_rpc::Client::new(&url, auth)?;

        // Verify connection
        runtime.block_on(retry("connect to bitcoind", || async {
            client
                .get_blockchain_info()
                .context("bitcoind not reachable")
        }))?;

        Ok(client)
    }

    /// Bring up a client daemon of its own, joined to the mint.
    pub async fn new_client(&self) -> anyhow::Result<TestClient> {
        self.new_client_at(&self.invite).await
    }

    /// Bring up a client daemon of its own, joined to the mint behind
    /// `invite`.
    pub async fn new_client_at(&self, invite: &InviteCode) -> anyhow::Result<TestClient> {
        let n = self.client_counter.fetch_add(1, Ordering::Relaxed);
        start_client(&self.data_dir, n, invite).await
    }

    pub fn mine_blocks(&self, n: u64) {
        block_in_place(|| self.bitcoind.generate_to_address(n, &dummy_address())).unwrap();
    }

    pub fn send_to_address(
        &self,
        addr: &bitcoin::Address,
        amount: bitcoin::Amount,
    ) -> anyhow::Result<bitcoin::Txid> {
        Ok(block_in_place(|| {
            self.bitcoind
                .send_to_address(addr, amount, None, None, None, None, None, None)
        })?)
    }
}

/// Spawn a client daemon against a fresh data dir, wait for its admin
/// socket and join the mint. The daemon picks a free port for its iroh
/// endpoint, as nothing dials a client.
async fn start_client(base: &Path, n: u64, invite: &InviteCode) -> anyhow::Result<TestClient> {
    let data_dir = base.join(format!("client-{n}"));
    tokio::fs::create_dir_all(&data_dir).await?;

    let log_file = std::fs::File::create(base.join(format!("client-{n}.log")))?;

    let child = Command::new("target/release/picomint-client-daemon")
        .env("DATA_DIR", &data_dir)
        .env("NETWORK", "regtest")
        .env("API_ADDR", "0.0.0.0:0")
        .env("INTEGRATION_TEST", "true")
        .stdout(log_file.try_clone()?)
        .stderr(log_file)
        .spawn()
        .context(format!("Failed to start client-{n}"))?;

    let client = TestClient::new(data_dir, invite.mint, child);

    retry(&format!("client-{n} admin socket"), || async {
        client.list()
    })
    .await?;

    client.add(invite)?;

    info!("Started client-{n}");
    Ok(client)
}

async fn start_node(base: &Path, node: usize) -> anyhow::Result<Child> {
    let p2p_port = NODE_BASE_PORT + (node as u16 * PORTS_PER_NODE);

    let data_dir = base.join(format!("node-{node}"));
    tokio::fs::create_dir_all(&data_dir).await?;

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(base.join(format!("node-{node}.log")))?;

    let child = Command::new("target/release/picomint-node-daemon")
        .env("DATA_DIR", data_dir.to_str().unwrap())
        .env("BITCOIND_URL", bitcoind_url())
        .env("P2P_ADDR", format!("127.0.0.1:{p2p_port}"))
        .env("INTEGRATION_TEST", "true")
        .stdout(log_file.try_clone()?)
        .stderr(log_file)
        .spawn()
        .context(format!("Failed to start node-{node}"))?;

    info!("Started node-{node} on port {p2p_port}");
    Ok(child)
}

async fn start_lnurl_daemon(base: &Path, port: u16) -> anyhow::Result<()> {
    let log_file = std::fs::File::create(base.join("lnurl-daemon.log"))?;

    Command::new("target/release/picomint-lnurl-daemon")
        .env("API_ADDR", format!("127.0.0.1:{port}"))
        .stdout(log_file.try_clone()?)
        .stderr(log_file)
        .spawn()
        .context("Failed to start picomint-lnurl-daemon")?;

    Ok(())
}

async fn start_gateway(
    base: &Path,
    name: &str,
    gateway_port: u16,
    lightning_port: u16,
) -> anyhow::Result<()> {
    let data_dir = base.join(name);

    tokio::fs::create_dir_all(&data_dir).await?;

    let log_file = std::fs::File::create(base.join(format!("{name}.log")))?;

    Command::new("target/release/picomint-gateway-daemon")
        .env("INTEGRATION_TEST", "true")
        .env("DATA_DIR", data_dir.to_str().unwrap())
        .env("API_ADDR", format!("0.0.0.0:{gateway_port}"))
        .env("LDK_ADDR", format!("0.0.0.0:{lightning_port}"))
        .env("NETWORK", "regtest")
        .env("BITCOIND_URL", bitcoind_url())
        .stdout(log_file.try_clone()?)
        .stderr(log_file)
        .spawn()
        .context(format!("Failed to start {name}"))?;

    info!("Started {name} on port {gateway_port}");
    Ok(())
}

/// Spawn a broker daemon against a fresh data dir; the caller waits for
/// its admin socket.
async fn start_broker(base: &Path, name: &str, port: u16) -> anyhow::Result<()> {
    let data_dir = base.join(name);

    tokio::fs::create_dir_all(&data_dir).await?;

    let log_file = std::fs::File::create(base.join(format!("{name}.log")))?;

    Command::new("target/release/picomint-broker-daemon")
        .env("INTEGRATION_TEST", "true")
        .env("DATA_DIR", data_dir.to_str().unwrap())
        .env("API_ADDR", format!("0.0.0.0:{port}"))
        .env("NETWORK", "regtest")
        .stdout(log_file.try_clone()?)
        .stderr(log_file)
        .spawn()
        .context(format!("Failed to start {name}"))?;

    info!("Started {name} on port {port}");
    Ok(())
}

/// Deposit into the broker's balance in `mint` and wait for the mint to
/// credit it.
async fn fund_broker(
    bitcoind: &bitcoincore_rpc::Client,
    broker_data_dir: &Path,
    mint: picomint_core::config::MintId,
) -> anyhow::Result<()> {
    let address = retry("broker deposit address", || async {
        cli::broker_onchain_receive(broker_data_dir, &mint.to_string())
    })
    .await?
    .address
    .assume_checked();

    block_in_place(|| {
        bitcoind.send_to_address(
            &address,
            bitcoin::Amount::from_sat(100_000_000),
            None,
            None,
            None,
            None,
            None,
            None,
        )
    })?;

    block_in_place(|| bitcoind.generate_to_address(10, &dummy_address()))?;

    retry("broker balance", || async {
        let balance = cli::broker_mint_balance(broker_data_dir, &mint.to_string())?.balance_msat;
        ensure!(balance > picomint_core::Amount::ZERO, "Balance is zero");
        Ok(())
    })
    .await
}

async fn run_dkg(node_data_dirs: &[std::path::PathBuf], mint_name: &str) -> anyhow::Result<()> {
    use picomint_node_cli_core::NodeStatus;

    // Wait for all nodes to be ready (the CLI `status` call returns once
    // the daemon has bound its CLI socket).
    for (node, data_dir) in node_data_dirs.iter().enumerate() {
        retry(&format!("node-{node} awaiting init"), || async {
            match cli::node_status(data_dir)? {
                NodeStatus::Setup(phase) => {
                    ensure!(phase.setup_code.is_none(), "node already initialised");
                    Ok(())
                }
                status => anyhow::bail!("Unexpected status: {status:?}"),
            }
        })
        .await?;
    }
    info!("All nodes awaiting init");

    // Initialize the nodes: node 0 is leader, rest are followers
    let mut setup_codes = BTreeMap::new();
    for (node, data_dir) in node_data_dirs.iter().enumerate() {
        let name = format!("Node {node}");
        let (mint_name, mint_size) = if node == 0 {
            (Some(mint_name), Some(node_data_dirs.len() as u8))
        } else {
            (None, None)
        };
        let resp = cli::node_setup_init(data_dir, &name, mint_name, mint_size)?;
        let setup_code = resp
            .get("setup_code")
            .and_then(|v| v.as_str())
            .context("missing setup_code in init response")?
            .to_string();
        setup_codes.insert(node, setup_code);
    }
    info!("All nodes initialized");

    // Exchange setup codes between all nodes
    for (node, code) in &setup_codes {
        for (other_node, data_dir) in node_data_dirs.iter().enumerate() {
            if other_node == *node {
                continue;
            }
            cli::node_setup_add(data_dir, code)?;
        }
    }
    info!("Node info exchanged");

    // Start DKG on all nodes
    for data_dir in node_data_dirs {
        cli::node_setup_confirm(data_dir)?;
    }

    info!("DKG started");
    Ok(())
}

fn build_ldk_node(
    base: &Path,
    runtime: Arc<tokio::runtime::Runtime>,
) -> anyhow::Result<Arc<ldk_node::Node>> {
    let mut builder = ldk_node::Builder::new();

    builder.set_runtime(runtime.handle().clone());
    builder.set_network(Network::Regtest);
    builder.set_node_alias("test-ldk-node".to_string())?;
    builder.set_listening_addresses(vec![
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, TEST_LDK_PORT).into(),
    ])?;
    builder.set_storage_dir_path(
        base.join("test-ldk-node")
            .to_str()
            .context("ldk storage path")?
            .to_string(),
    );
    builder.set_chain_source_bitcoind_rpc(
        "127.0.0.1".to_string(),
        BTC_RPC_PORT,
        BTC_RPC_USER.to_string(),
        BTC_RPC_PASS.to_string(),
    );

    let node = Arc::new(builder.build()?);
    node.start()?;

    Ok(node)
}

async fn open_channel(
    bitcoind: &bitcoincore_rpc::Client,
    gateway_data_dir: &std::path::Path,
    ldk_node: &ldk_node::Node,
) -> anyhow::Result<()> {
    let addr = cli::gateway_ldk_onchain_receive(gateway_data_dir)?
        .address
        .assume_checked();

    block_in_place(|| bitcoind.generate_to_address(1, &addr))?;
    block_in_place(|| bitcoind.generate_to_address(100, &dummy_address()))?;

    let target_height = block_in_place(|| bitcoind.get_block_count())? - 1;
    retry("gateway sync", || async {
        let info = cli::gateway_info(gateway_data_dir)?;
        ensure!(
            info.block_height >= target_height,
            "not synced: {} < {target_height}",
            info.block_height,
        );
        Ok(())
    })
    .await?;

    let ldk_pubkey = ldk_node.node_id().to_string();
    let ldk_lightning_addr = format!("127.0.0.1:{TEST_LDK_PORT}");

    cli::gateway_ldk_channel_open(
        gateway_data_dir,
        &ldk_pubkey,
        &ldk_lightning_addr,
        bitcoin::Amount::from_sat(10_000_000),
        bitcoin::Amount::from_sat(5_000_000),
    )?;

    // Wait for the funding tx to be negotiated
    let funding_txid = retry("funding tx", || async {
        cli::gateway_ldk_channel_list(gateway_data_dir)?
            .channels
            .into_iter()
            .find_map(|c| c.funding_txid)
            .context("no funding txid yet")
    })
    .await?;

    // Wait for the funding tx to be broadcast. The background miner may
    // confirm it out of the mempool between polls, and bitcoind runs
    // without txindex — so probe the (certainly unspent) funding outputs
    // for the confirmed case.
    retry("funding tx broadcast", || async {
        let in_mempool = block_in_place(|| bitcoind.get_mempool_entry(&funding_txid)).is_ok();

        let confirmed = (0..2).any(|vout| {
            block_in_place(|| bitcoind.get_tx_out(&funding_txid, vout, Some(false)))
                .ok()
                .flatten()
                .is_some()
        });

        ensure!(
            in_mempool || confirmed,
            "funding tx neither in mempool nor confirmed"
        );

        Ok(())
    })
    .await?;

    // Mine to confirm channel
    block_in_place(|| bitcoind.generate_to_address(10, &dummy_address()))?;

    // Wait for channel to be active on the gateway side
    retry("channel active", || async {
        let channels = cli::gateway_ldk_channel_list(gateway_data_dir)?.channels;
        ensure!(
            channels.iter().any(|c| c.is_usable),
            "no active channels yet"
        );
        Ok(())
    })
    .await?;

    Ok(())
}

pub async fn retry<F, Fut, T>(name: &str, f: F) -> anyhow::Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    for i in 0..720 {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if i == 719 {
                    return Err(e).context(format!("retry '{name}' exhausted after 720 attempts"));
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
    unreachable!()
}
