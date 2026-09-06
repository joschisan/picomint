#![warn(missing_docs)]
//! The Picomint client daemon: the client library behind an admin socket.
//!
//! A headless client for machines — load generation, latency measurement,
//! agents. Every route is one client call with the same arguments and the
//! same return shape; an operation's outcome is read from the analytics
//! mirror of the event log through `query`, never awaited here.

mod cli;
mod db;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use bitcoin::Network;
use clap::Parser;
use iroh::endpoint::presets::N0;
use iroh_mdns_address_lookup::MdnsAddressLookup;
use picomint_analytics::Analytics;
use picomint_client::{Client, Mnemonic};
use tracing::info;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Command line parameters for starting the client daemon.
#[derive(Parser)]
#[command(version)]
pub struct ClientOpts {
    /// Path to folder containing the daemon's data files
    #[arg(long = "data-dir", env = "DATA_DIR")]
    pub data_dir: PathBuf,

    /// Bitcoin network every added mint must run on
    #[arg(long = "network", env = "NETWORK", default_value = "bitcoin")]
    pub network: Network,

    /// Listen address for the iroh endpoint the mint and gateway traffic
    /// goes out of
    #[arg(long = "api-addr", env = "API_ADDR", default_value = "0.0.0.0:8080")]
    pub api_addr: SocketAddr,
}

/// Everything the admin handlers reach for.
#[derive(Clone)]
pub struct AppState {
    /// The client every route wraps.
    pub client: Arc<Client>,
    /// The seed words the client derives from.
    pub mnemonic: Mnemonic,
    /// Where the database, the analytics and the socket live.
    pub data_dir: PathBuf,
    /// The network every added mint must run on.
    pub network: Network,
    /// The analytics writer the trailer feeds.
    pub analytics: Analytics,
}

fn main() -> anyhow::Result<()> {
    tokio_rustls::rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .try_init()?;

    let opts = ClientOpts::parse();

    let db = picomint_redb::Database::open(opts.data_dir.join(db::DB_FILE))?;

    let mnemonic = db::load_or_init_mnemonic(&db)?;

    let runtime = Arc::new(tokio::runtime::Runtime::new()?);

    let endpoint = runtime.block_on(
        iroh::Endpoint::builder(N0)
            .alpns(vec![picomint_rpc::ALPN.to_vec()])
            .transport_config(picomint_rpc::transport_config())
            .bind_addr(opts.api_addr)?
            .address_lookup(MdnsAddressLookup::builder())
            .bind(),
    )?;

    // `Client::new` brings every added mint up, which spawns their
    // background tasks — so the runtime stays entered from here on.
    let _rt = runtime.enter();

    let client = Arc::new(Client::new(endpoint, db, mnemonic.clone()));

    let state = AppState {
        client,
        mnemonic,
        data_dir: opts.data_dir.clone(),
        network: opts.network,
        analytics: Analytics::wipe_and_init(&opts.data_dir)?,
    };

    runtime.spawn(cli::run(state.clone()));

    runtime.spawn(picomint_analytics::trailer(
        state.client.clone(),
        state.analytics.clone(),
    ));

    runtime.block_on(shutdown_signal());

    info!("Client daemon exiting...");

    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("Failed to install SIGTERM handler")
        .recv()
        .await;
}
