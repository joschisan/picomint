mod cli;
mod ecash;
mod env;
mod expiry;
mod lightning;
mod onchain;
mod restore;

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

    runtime.block_on(client_send.client.shutdown());

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

    info!(
        total_ms = t_total.elapsed().as_millis() as u64,
        "All integration tests passed!"
    );

    if std::env::var_os("KEEP_ALIVE").is_some() {
        return keep_alive(&runtime, &env);
    }

    std::process::exit(0);
}

/// Keep the mint running after the suite passes so it can be driven by
/// hand — pair a phone with the printed invite, or hit the daemons with
/// `picomint-{node,gateway}-cli --data-dir <dir>`. Blocks until Ctrl-C;
/// the wrapper script tears the daemons down on exit.
fn keep_alive(runtime: &tokio::runtime::Runtime, env: &env::TestEnv) -> anyhow::Result<()> {
    let base = &env.data_dir;
    let g0 = base.join("node-0");

    // The lightning suite registers then deregisters the gateway as cleanup, so
    // re-register the real gateway with every node here — otherwise the
    // kept-alive mint exposes no gateway and a paired phone can't do
    // Lightning.
    info!("Registering gateway with all nodes");
    for node in 0..env::NUM_NODES {
        cli::node_lightning_gateway_add(&cli::node_data_dir(base, node), &env.gateway_pk)?;
    }

    println!();
    println!("==========================================================================");
    println!(" picomint local devnet is UP — keep this process running");
    println!("==========================================================================");
    println!();
    println!(" Invite (pair your phone):");
    println!("   {}", picomint_base32::encode(&env.invite));
    println!();
    println!(" Nodes (picomint-node-cli --data-dir <dir> <cmd>):");
    for i in 0..env::NUM_NODES as u16 {
        let ui_port = env::NODE_BASE_PORT + i * env::PORTS_PER_NODE + 1;
        println!(
            "   node-{i}: {}   (UI http://127.0.0.1:{ui_port}, password: test)",
            base.join(format!("node-{i}")).display(),
        );
    }
    println!();
    println!(" Gateway (picomint-gateway-cli --data-dir <dir> <cmd>):");
    println!("   {}", env.gateway_data_dir.display());
    println!();
    println!(" Examples:");
    println!(
        "   target/release/picomint-node-cli --data-dir {} invite",
        g0.display(),
    );
    println!(
        "   target/release/picomint-node-cli --data-dir {} session-count",
        g0.display(),
    );
    println!(
        "   target/release/picomint-gateway-cli  --data-dir {} info",
        env.gateway_data_dir.display(),
    );
    println!();
    println!(" Ctrl-C to tear everything down.");
    println!("==========================================================================");

    info!("Mint up; waiting for Ctrl-C…");
    runtime.block_on(async {
        let _ = tokio::signal::ctrl_c().await;
    });
    info!("Ctrl-C received; shutting down devnet");

    Ok(())
}
