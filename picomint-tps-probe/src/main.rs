//! Measures sustained transaction throughput against a live federation.
//!
//! Each worker runs a closed loop — `send` a bundle to itself, `receive` it
//! back, wait for the notes to land — and repeats for a fixed duration. That
//! keeps every worker's funds recycling, so a run is bounded by time and fees
//! rather than by how much ecash was staked up front.
//!
//! Throughput is counted from the event log rather than from the loop, because
//! a `send` only submits a transaction when the wallet lacks exact change: the
//! loop knows how many *round trips* it completed, and only `TxAcceptEvent`
//! knows how many transactions the federation actually ordered.
//!
//! ```text
//! picomint-tps-probe --invite picomint... --fund-ecash picomint... \
//!     --workers 8 --duration 30
//! ```
//!
//! Get a funding bundle from a gateway with a balance on the federation:
//!
//! ```text
//! picomint-gateway-cli federation module mint send '500000 sat' --id <mint>
//! ```

use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use clap::Parser;
use futures::StreamExt;
use iroh::Endpoint;
use iroh::endpoint::presets::N0;
use picomint_client::mint::{ECash, MintSuccessEvent, SendECashError};
use picomint_client::{Client, Mnemonic, OperationId, TxAcceptEvent, TxRejectEvent};
use picomint_core::Amount;
use picomint_core::invite::InviteCode;
use picomint_eventlog::{EventLogEntry, EventLogId, EventLogger};
use picomint_redb::table;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

table!(EventLogTable, EventLogId => EventLogEntry, "event-log");
table!(
    EventLogByOperationTable,
    (OperationId, EventLogId) => EventLogEntry,
    "operation-event-log",
);

#[derive(Parser)]
#[command(about = "Measures sustained transaction throughput against a live federation")]
struct Opts {
    /// Federation invite code.
    #[arg(long)]
    invite: String,

    /// Out-of-band ecash to stake the probe wallet with. Must be from the same
    /// federation as `--invite`, and large enough that per-transaction fees do
    /// not drain it before the run ends.
    #[arg(long)]
    fund_ecash: String,

    /// Concurrent send/receive loops. Each is one transaction in flight at a
    /// time, so this is the offered concurrency.
    #[arg(long, default_value_t = 8)]
    workers: usize,

    /// Seconds to sustain load for, after warmup.
    #[arg(long, default_value_t = 30)]
    duration: u64,

    /// Amount each loop moves, in sats.
    #[arg(long, default_value_t = 100)]
    amount_sat: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::WARN.into())
                .from_env_lossy(),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .try_init()?;

    let opts = Opts::parse();

    let invite: InviteCode =
        picomint_base32::decode(&opts.invite).context("Failed to decode invite code")?;
    let fund = ECash::from_str(&opts.fund_ecash).context("Failed to decode fund ecash")?;

    let endpoint = Endpoint::builder(N0).bind().await?;

    let dir = tempfile::TempDir::new()?.keep();
    let db = picomint_redb::Database::open(dir.join("database.redb"))?;

    let config = picomint_client::download(&endpoint, &invite).await?;
    let logger = EventLogger::new(EventLogTable, EventLogByOperationTable);
    let client = Client::new(
        endpoint.clone(),
        db,
        logger,
        &Mnemonic::generate(12)?,
        config,
    );

    let operation = client.mint().receive(&fund)?;
    settle(&client, operation).await?;

    let staked = client.get_balance();
    info!("Staked {staked}");

    let amount = Amount::from_sat(opts.amount_sat);

    // One untimed lap per worker. It pays the first-transaction costs — the
    // connection handshakes and whatever change the wallet has to mint before
    // it holds the exact denominations a send wants — none of which belong in
    // a steady-state throughput number.
    warmup(&client, opts.workers, amount).await?;

    let started_at = drain_log(&client).await;
    let stats = Arc::new(Stats::default());

    println!(
        "\nrunning {} workers for {}s at {amount} per loop...",
        opts.workers, opts.duration
    );

    let clock = Instant::now();
    let deadline = clock + Duration::from_secs(opts.duration);

    let workers = (0..opts.workers).map(|_| {
        let client = client.clone();
        let stats = stats.clone();

        tokio::spawn(async move {
            while Instant::now() < deadline {
                match round_trip(&client, amount, &stats).await {
                    Ok(()) => stats.round_trips.fetch_add(1, Ordering::Relaxed),
                    Err(e) => {
                        warn!(error = %e, "Round trip failed");
                        stats.failures.fetch_add(1, Ordering::Relaxed)
                    }
                };
            }
        })
    });

    futures::future::join_all(workers).await;

    let elapsed = clock.elapsed().as_secs_f64();
    let transactions = count_accepted(&client, started_at).await;
    let round_trips = stats.round_trips.load(Ordering::Relaxed);
    let failures = stats.failures.load(Ordering::Relaxed);
    let backpressure = stats.backpressure.load(Ordering::Relaxed);

    println!("\n=== throughput ===");
    println!("  workers            {:>10}", opts.workers);
    println!("  elapsed            {elapsed:>10.2}s");
    println!("  transactions       {transactions:>10}   (accepted by the federation)");
    println!("  round trips        {round_trips:>10}   (send + receive + settle)");
    println!("  failures           {failures:>10}");
    println!("  backpressure       {backpressure:>10}   (sends waiting on notes to settle)");
    println!();
    println!(
        "  transactions/sec   {:>10.2}",
        transactions as f64 / elapsed
    );
    println!(
        "  round trips/sec    {:>10.2}",
        round_trips as f64 / elapsed
    );
    if round_trips > 0 {
        println!(
            "  mean round trip    {:>10.0}ms   at {} concurrent",
            elapsed * 1000.0 * opts.workers as f64 / round_trips as f64,
            opts.workers
        );
    }
    println!("  fees paid          {}", staked - client.get_balance());
    println!();

    client.shutdown().await;

    Ok(())
}

/// One send/receive/settle lap. The unit of offered load.
///
/// A `send` that reports `InsufficientBalance` has not found a real shortfall
/// — the wallet's notes are tied up in laps that have not settled yet, and
/// they come back. That is the system pushing back, so it is waited out and
/// counted separately rather than recorded as a failure, which is also what
/// keeps a worker count above the wallet's note supply from simply erroring
/// out.
async fn round_trip(client: &Arc<Client>, amount: Amount, stats: &Stats) -> anyhow::Result<()> {
    let ecash = loop {
        match client.mint().send(amount).await {
            Ok(ecash) => break ecash,
            Err(SendECashError::InsufficientBalance) => {
                stats.backpressure.fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(e) => return Err(e.into()),
        }
    };

    let operation = client.mint().receive(&ecash)?;

    settle(client, operation).await
}

/// Shared counters for one run.
#[derive(Default)]
struct Stats {
    round_trips: AtomicU64,
    failures: AtomicU64,
    backpressure: AtomicU64,
}

/// Lap once per worker, sequentially. Establishes the connections and — more
/// importantly — fragments the staked balance, which arrives as roughly one
/// note per denomination, into a spread with enough notes to keep every worker
/// funded once they run concurrently.
async fn warmup(client: &Arc<Client>, workers: usize, amount: Amount) -> anyhow::Result<()> {
    let stats = Stats::default();

    for _ in 0..workers {
        round_trip(client, amount, &stats).await?;
    }

    Ok(())
}

async fn settle(client: &Arc<Client>, operation: OperationId) -> anyhow::Result<()> {
    let mut stream = client.subscribe_operation_events(operation);
    let mut accepted = false;

    while let Some(entry) = stream.next().await {
        if entry.to_event::<TxAcceptEvent>().is_some() {
            accepted = true;
        }

        if let Some(event) = entry.to_event::<TxRejectEvent>() {
            bail!("Transaction rejected: {}", event.error);
        }

        if accepted && entry.to_event::<MintSuccessEvent>().is_some() {
            return Ok(());
        }
    }

    bail!("Event stream ended before the operation settled")
}

/// Walk to the end of the event log and return the next position, so the run
/// counts only what it produces itself and not the staking and warmup laps.
async fn drain_log(client: &Arc<Client>) -> EventLogId {
    let mut pos = EventLogId::LOG_START;

    loop {
        let batch = client.get_event_log(pos, 1000).await;

        match batch.last() {
            Some((id, _)) => pos = id.saturating_add(1),
            None => return pos,
        }
    }
}

/// Transactions the federation actually ordered since `from`, which is the
/// only honest denominator — a send that finds exact change submits nothing.
async fn count_accepted(client: &Arc<Client>, from: EventLogId) -> u64 {
    let mut pos = from;
    let mut accepted = 0;

    loop {
        let batch = client.get_event_log(pos, 1000).await;

        let Some((id, _)) = batch.last() else {
            return accepted;
        };

        pos = id.saturating_add(1);

        accepted += batch
            .iter()
            .filter(|(_, entry)| entry.to_event::<TxAcceptEvent>().is_some())
            .count() as u64;
    }
}
