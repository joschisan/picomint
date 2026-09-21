#![warn(missing_docs)]
//! The Picomint broker daemon: a client of several mints that funds swaps
//! between them.
//!
//! It serves Picomint clients over a public iroh endpoint (consensus-encoded
//! `BrokerMethod` requests) and is managed through a separate
//! HTTP-over-Unix-socket admin CLI.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::ensure;
use bitcoin::Network;
use clap::Parser;
use iroh::endpoint::presets::N0;
use iroh_mdns_address_lookup::MdnsAddressLookup;
use picomint_broker_daemon::{AppState, DB_FILE, cli, db, public};
use picomint_core::Amount;
use picomint_core::lightning::gateway::PaymentFee;
use picomint_core::swap::broker::BrokerInfo;
use tracing::info;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Command line parameters for starting the broker.
#[derive(Parser)]
#[command(version)]
pub struct BrokerOpts {
    /// Path to folder containing broker config and data files
    #[arg(long = "data-dir", env = "DATA_DIR")]
    pub data_dir: PathBuf,

    /// Bitcoin network every added mint must run on
    #[arg(long = "network", env = "NETWORK")]
    pub network: Network,

    /// Public API listen address. The iroh endpoint binds here for the
    /// broker API and outgoing mint client traffic.
    #[arg(long = "api-addr", env = "API_ADDR", default_value = "0.0.0.0:8080")]
    pub api_addr: SocketAddr,

    /// Base fee in millisatoshis: the broker's cut on every swap.
    #[arg(long, env = "FEE_BASE_MSAT", default_value_t = 10_000)]
    pub fee_base_msat: u64,

    /// Fee rate in parts per million: the broker's cut on every swap.
    #[arg(long, env = "FEE_PPM", default_value_t = 3000)]
    pub fee_ppm: u16,
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

    let opts = BrokerOpts::parse();

    // Clients enforce this limit on every swap, so a broker configured
    // above it would be rejected by every client; fail fast at startup
    // instead.
    let fee = PaymentFee {
        base: Amount(opts.fee_base_msat),
        ppm: opts.fee_ppm,
    };

    ensure!(
        fee.is_within(&BrokerInfo::FEE_LIMIT),
        "Configured fee {fee:?} exceeds the limit clients accept {:?}",
        BrokerInfo::FEE_LIMIT,
    );

    let db = picomint_redb::Database::open(opts.data_dir.join(DB_FILE))?;

    let mnemonic = db::load_or_init_mnemonic(&db)?;

    let iroh_secret_key = db::load_or_init_iroh_secret_key(&db);

    let runtime = Arc::new(tokio::runtime::Runtime::new()?);

    let endpoint = runtime.block_on(
        iroh::Endpoint::builder(N0)
            .secret_key(iroh_secret_key)
            .alpns(vec![picomint_rpc::ALPN.to_vec()])
            .transport_config(picomint_rpc::transport_config())
            .bind_addr(opts.api_addr)?
            .address_lookup(MdnsAddressLookup::builder())
            .bind(),
    )?;

    // `Client::new_broker` brings every added mint up, which spawns their
    // background tasks — so the runtime stays entered from here on.
    let _rt = runtime.enter();

    let client = Arc::new(picomint_client::Client::new_broker(
        endpoint.clone(),
        db.clone(),
        mnemonic.clone(),
    ));

    let state = AppState {
        client,
        endpoint: endpoint.clone(),
        mnemonic,
        db,
        data_dir: opts.data_dir.clone(),
        network: opts.network,
        fee,
        analytics: picomint_analytics::Analytics::wipe_and_init(&opts.data_dir)?,
    };

    runtime.spawn(public::run(state.clone(), endpoint));

    runtime.spawn(cli::run(state.clone())?);

    runtime.spawn(picomint_analytics::trailer(
        state.client.clone(),
        state.analytics.clone(),
    ));

    runtime.block_on(shutdown_signal());

    info!("Broker daemon exiting...");

    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("Failed to install SIGTERM handler")
        .recv()
        .await;
}
