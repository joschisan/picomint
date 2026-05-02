mod cli;
mod env;
mod ln;
mod mint;
mod recover;
mod wallet;

use std::sync::Arc;

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
    info!("Invite code: {}", picomint_base32::encode(&env.invite_code));
    info!("Gateway: {}", env.gw_data_dir.display());

    info!("Running wallet tests...");
    runtime.block_on(wallet::run_tests(&env, &client_send))?;

    info!("Running ln + mint tests in parallel...");
    runtime.block_on(async {
        tokio::try_join!(
            ln::run_tests(&env, &client_send),
            mint::run_tests(&env, &client_send),
        )
    })?;

    info!("Shutting down the primary test client!");

    runtime.block_on(client_send.shutdown());

    info!("Running guardian backup/recover test...");
    runtime.block_on(recover::run_test(&env))?;

    info!(
        total_ms = t_total.elapsed().as_millis() as u64,
        "All integration tests passed!"
    );

    std::process::exit(0);
}
