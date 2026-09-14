mod cli;
mod client;
mod ecash;
mod env;
mod expiry;
mod lightning;
mod onchain;
mod restore;
mod rugpull;

use std::sync::Arc;

use anyhow::ensure;
use tracing::info;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .try_init()?;

    let runtime = Arc::new(tokio::runtime::Runtime::new()?);

    let t_total = std::time::Instant::now();

    info!("Setting up test environment...");
    let (env, client_send) = env::TestEnv::setup(runtime.clone())?;

    info!("Test environment ready!");
    info!("Invite code: {}", picomint_base32::encode(&env.invite));
    info!("Gateway: {}", env.gateway_data_dir.display());

    info!("Running onchain tests...");
    runtime.block_on(onchain::run_tests(&env, &client_send))?;

    info!("Running lightning + ecash tests in parallel...");
    runtime.block_on(async {
        tokio::try_join!(
            lightning::run_tests(&env, &client_send),
            ecash::run_tests(&env, &client_send),
        )
    })?;

    info!("Running expiry test...");
    runtime.block_on(expiry::run_test(&env))?;

    info!("Shutting down the primary test client!");

    runtime.block_on(client_send.shutdown());

    info!("Removing the mint from the gateway...");
    cli::gateway_mint_remove(&env.gateway_data_dir, &env.invite.mint.to_string())?;

    ensure!(
        cli::gateway_mint_list(&env.gateway_data_dir)?
            .mints
            .is_empty(),
        "gateway still lists mints after remove"
    );

    info!("Running node backup/restore test...");
    runtime.block_on(restore::run_test(&env))?;

    info!("Running rugpull test...");
    runtime.block_on(rugpull::run_test(&env))?;

    info!(
        total_ms = t_total.elapsed().as_millis() as u64,
        "All integration tests passed!"
    );

    std::process::exit(0);
}
