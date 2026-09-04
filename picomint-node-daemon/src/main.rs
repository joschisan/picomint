//! `picomint-node-daemon` process entry point.
//!
//! Parses CLI arguments, opens the database, wires up the bitcoin RPC, and
//! hands off to [`picomint_node_daemon::run_server`].

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::ensure;
use bitcoin::Network;
use clap::Parser;
use picomint_node_daemon::bitcoind::BitcoindClient;
use picomint_node_daemon::config::ConfigGenSettings;
use picomint_node_daemon::{DB_FILE, run_server};
use tracing::info;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use url::Url;

#[derive(Parser)]
#[command(version)]
struct ServerOpts {
    /// Path to folder containing mint config files
    #[arg(long = "data-dir", env = "DATA_DIR")]
    data_dir: PathBuf,

    /// The bitcoin network of the mint
    #[arg(long, env = "BITCOIN_NETWORK", default_value = "bitcoin")]
    bitcoin_network: Network,

    /// Bitcoind RPC URL with embedded credentials, e.g.
    /// `http://user:pass@127.0.0.1:8332`. The node must be unpruned —
    /// the node needs random access to any block from mint
    /// start onward to make consensus progress.
    #[arg(long, env = "BITCOIND_URL")]
    bitcoind_url: Url,

    /// Address we bind to for iroh (p2p consensus + client API)
    #[arg(long = "p2p-addr", env = "P2P_ADDR", default_value = "0.0.0.0:8080")]
    p2p_addr: SocketAddr,

    /// Listen address for the Web UI. The UI is unauthenticated; bind it
    /// to loopback (the default) or expose it via SSH tunnel / VPN. See
    /// README.md.
    #[arg(long = "ui-addr", env = "UI_ADDR", default_value = "127.0.0.1:3000")]
    ui_addr: SocketAddr,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let picomint_version = env!("CARGO_PKG_VERSION");

    let server_opts = ServerOpts::parse();

    ensure!(
        server_opts.bitcoind_url.password().is_some(),
        "BITCOIND_URL must embed credentials: http://user:pass@host"
    );

    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .try_init()
        .unwrap();

    info!("Starting picomint-node-daemon (version: {picomint_version})");

    let db = picomint_redb::Database::open(server_opts.data_dir.join(DB_FILE))
        .expect("Failed to open picomint-node-daemon database");

    let settings = ConfigGenSettings {
        p2p_addr: server_opts.p2p_addr,
        ui_addr: server_opts.ui_addr,
        network: server_opts.bitcoin_network,
        data_dir: server_opts.data_dir,
    };

    // The reqwest client inside `BitcoindClient` requires an installed
    // rustls crypto provider at construction time.
    tokio_rustls::rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let bitcoind = Arc::new(BitcoindClient::new(server_opts.bitcoind_url));

    // Run consensus on the main task. Inner spawned tasks are fire-and-forget
    // — process death (SIGTERM/SIGKILL) is the shutdown protocol; db commits
    // are atomic and BFT sessions resume from disk on next boot. The only
    // graceful return path is the mint-shutdown-via-API mechanism, which
    // unwinds the engine cleanly.
    run_server(settings, db, bitcoind).await
}
