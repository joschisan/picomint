mod cli;
mod env;
mod expiry;
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
    info!("Invite code: {}", picomint_base32::encode(&env.invite));
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

    info!("Running expiry test...");
    runtime.block_on(expiry::run_test(&env))?;

    info!("Shutting down the primary test client!");

    runtime.block_on(client_send.shutdown());

    info!("Running guardian backup/recover test...");
    runtime.block_on(recover::run_test(&env))?;

    info!(
        total_ms = t_total.elapsed().as_millis() as u64,
        "All integration tests passed!"
    );

    if std::env::var_os("KEEP_ALIVE").is_some() {
        return keep_alive(&runtime, &env);
    }

    std::process::exit(0);
}

/// Keep the federation running after the suite passes so it can be driven by
/// hand — pair a phone with the printed invite, or hit the daemons with
/// `picomint-{guardian,gateway}-cli --data-dir <dir>`. Blocks until Ctrl-C;
/// the wrapper script tears the daemons down on exit.
fn keep_alive(runtime: &tokio::runtime::Runtime, env: &env::TestEnv) -> anyhow::Result<()> {
    let base = &env.data_dir;
    let g0 = base.join("guardian-0");

    // The ln suite registers then deregisters the gateway as cleanup, so
    // re-register the real gateway with every guardian here — otherwise the
    // kept-alive federation exposes no gateway and a paired phone can't do
    // Lightning.
    info!("Registering gateway with all guardians");
    for peer in 0..env::NUM_GUARDIANS {
        cli::guardian_ln_gateway_add(&cli::guardian_data_dir(base, peer), &env.gw_pk)?;
    }

    println!();
    println!("==========================================================================");
    println!(" picomint local devnet is UP — keep this process running");
    println!("==========================================================================");
    println!();
    println!(" Invite (pair your phone):");
    println!("   {}", picomint_base32::encode(&env.invite));
    println!();
    println!(" Guardians (picomint-guardian-cli --data-dir <dir> <cmd>):");
    for i in 0..env::NUM_GUARDIANS as u16 {
        let ui_port = env::GUARDIAN_BASE_PORT + i * env::PORTS_PER_GUARDIAN + 1;
        println!(
            "   guardian-{i}: {}   (UI http://127.0.0.1:{ui_port}, password: test)",
            base.join(format!("guardian-{i}")).display(),
        );
    }
    println!();
    println!(" Gateway (picomint-gateway-cli --data-dir <dir> <cmd>):");
    println!("   {}", env.gw_data_dir.display());
    println!();
    println!(" Examples:");
    println!(
        "   target/release/picomint-guardian-cli --data-dir {} invite",
        g0.display(),
    );
    println!(
        "   target/release/picomint-guardian-cli --data-dir {} session-count",
        g0.display(),
    );
    println!(
        "   target/release/picomint-gateway-cli  --data-dir {} info",
        env.gw_data_dir.display(),
    );
    println!();
    println!(" Ctrl-C to tear everything down.");
    println!("==========================================================================");

    info!("Federation up; waiting for Ctrl-C…");
    runtime.block_on(async {
        let _ = tokio::signal::ctrl_c().await;
    });
    info!("Ctrl-C received; shutting down devnet");

    Ok(())
}
