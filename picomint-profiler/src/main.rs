//! Latency profiler: drives a real `picomint-client` against a remote
//! federation (by invite code) and times the operations a user actually
//! feels — ecash receive and Lightning send/receive (via a circular
//! self-pay). Run locally so the numbers include real client↔federation
//! network latency.

use std::collections::BTreeMap;
use std::time::Instant;

use anyhow::Context;
use clap::Parser;
use futures::StreamExt;
use iroh::Endpoint;
use iroh::endpoint::presets::N0;
use iroh_mdns_address_lookup::MdnsAddressLookup;
use lightning_invoice::Bolt11Invoice;
use picomint_client::ln::LightningClientModule;
use picomint_client::ln::events::{
    ReceiveEvent as LnReceiveEvent, SendRefundEvent, SendSuccessEvent,
};
use picomint_client::mint::{ECash, MintSuccessEvent};
use picomint_client::{Client, Mnemonic, OperationId, TxRejectEvent};
use picomint_core::Amount;
use picomint_core::PeerId;
use picomint_core::core::OperationId as CoreOperationId;
use picomint_core::invite::InviteCode;
use picomint_core::ln::gateway::GatewayPk;
use picomint_eventlog::{EventLogEntry, EventLogId, EventLogger};
use picomint_redb::{Database, table};
use tracing::info;
use tracing_subscriber::EnvFilter;

table!(EventLogTable, EventLogId => EventLogEntry, "event-log");
table!(
    EventLogByOperationTable,
    (CoreOperationId, EventLogId) => EventLogEntry,
    "operation-event-log",
);

#[derive(Parser)]
struct Args {
    /// Invite code of the federation to profile against.
    #[arg(long)]
    invite: InviteCode,
    /// OOB ecash string used to fund the profiling client (from the
    /// gateway's `mint send`).
    #[arg(long)]
    fund_ecash: String,
    /// Iterations per operation type.
    #[arg(long, default_value_t = 20)]
    iters: usize,
    /// Per-iteration ecash amount, sats.
    #[arg(long, default_value_t = 1000)]
    amount_sat: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    let endpoint = Endpoint::builder(N0)
        .address_lookup(MdnsAddressLookup::builder())
        .bind()
        .await?;

    let dir = tempfile::TempDir::new()?.keep();
    let db = Database::open(dir.join("database.redb"))?;
    let mnemonic = Mnemonic::generate(12)?;

    info!("downloading config from invite...");
    let config = picomint_client::download(&endpoint, &args.invite).await?;

    let logger = EventLogger::new(EventLogTable, EventLogByOperationTable);
    let client = Client::new(endpoint, db, logger, &mnemonic, config)?;

    // --- fund the client with the gateway-minted OOB ecash ---
    let ecash: ECash = args.fund_ecash.parse().context("parse fund ecash")?;
    let t = Instant::now();
    let op = client
        .mint()
        .receive(&ecash)
        .map_err(|e| anyhow::anyhow!("fund receive: {e:?}"))?;
    wait_mint_success(&client, op).await?;
    info!(fund_ms = t.elapsed().as_millis() as u64, balance = ?client.get_balance(), "funded");

    // --- gateways ---
    LightningClientModule::update_gateway_pks(client.ln().clone()).await?;
    LightningClientModule::update_gateway_info(client.ln().clone()).await;
    let (gw, _info) = client
        .ln()
        .select_gateway(None)
        .map_err(|e| anyhow::anyhow!("select gateway: {e:?} — is one registered/reachable?"))?;
    info!(gateway = %gw.0.fmt_short(), "gateway selected");

    // --- RTT sampling state ---
    // One consensus-free round-trip per guardian (liveness) and to the gateway
    // (info), taken after each timed op so it captures load during the run
    // without inflating the op timings.
    let peers: Vec<PeerId> = client.api().all_peers().into_iter().collect();
    let mut guardian_rtt: BTreeMap<PeerId, Vec<u64>> = BTreeMap::new();
    let mut gateway_rtt: Vec<u64> = Vec::new();

    // --- ecash receive ---
    let mut ecash_ms = Vec::new();
    for i in 0..args.iters {
        let notes = client
            .mint()
            .send(Amount::from_sat(args.amount_sat))
            .await
            .map_err(|e| anyhow::anyhow!("mint send: {e:?}"))?;
        let t = Instant::now();
        let op = client
            .mint()
            .receive(&notes)
            .map_err(|e| anyhow::anyhow!("mint receive: {e:?}"))?;
        wait_mint_success(&client, op).await?;
        let ms = t.elapsed().as_millis() as u64;
        ecash_ms.push(ms);
        info!(iter = i + 1, ecash_receive_ms = ms, "ecash receive");

        sample_rtt(&client, &peers, gw, &mut guardian_rtt, &mut gateway_rtt).await;
    }

    // --- circular Lightning send/receive ---
    let mut send_ms = Vec::new();
    let mut recv_ms = Vec::new();
    for i in 0..args.iters {
        let cursor = log_end(&client).await;

        let (rgw, rinfo) = client
            .ln()
            .select_gateway(None)
            .map_err(|e| anyhow::anyhow!("select gw (recv): {e:?}"))?;
        let t_recv = Instant::now();
        let invoice: Bolt11Invoice = client
            .ln()
            .receive(rgw, rinfo, Amount::from_sat(args.amount_sat), 300)
            .await
            .map_err(|e| anyhow::anyhow!("ln receive: {e:?}"))?;

        let (sgw, sinfo) = client
            .ln()
            .select_gateway(Some(&invoice))
            .map_err(|e| anyhow::anyhow!("select gw (send): {e:?}"))?;
        let t_send = Instant::now();
        let send_op = client
            .ln()
            .send(sgw, sinfo, invoice)
            .await
            .map_err(|e| anyhow::anyhow!("ln send: {e:?}"))?;
        wait_ln_send(&client, send_op).await?;
        let s_ms = t_send.elapsed().as_millis() as u64;
        send_ms.push(s_ms);

        wait_ln_receive(&client, cursor).await?;
        let r_ms = t_recv.elapsed().as_millis() as u64;
        recv_ms.push(r_ms);

        info!(
            iter = i + 1,
            ln_send_ms = s_ms,
            ln_receive_ms = r_ms,
            "ln circular"
        );

        sample_rtt(&client, &peers, gw, &mut guardian_rtt, &mut gateway_rtt).await;
    }

    report("ecash receive", &mut ecash_ms);
    report("ln send", &mut send_ms);
    report("ln receive (incl. send)", &mut recv_ms);

    for (peer, samples) in &mut guardian_rtt {
        report_rtt(&format!("rtt guardian {peer}"), samples);
    }
    report_rtt("rtt gateway", &mut gateway_rtt);

    client.shutdown().await;
    Ok(())
}

fn report(label: &str, v: &mut [u64]) {
    if v.is_empty() {
        return;
    }
    v.sort_unstable();
    let n = v.len();
    let pct = |p: usize| v[(n * p / 100).min(n - 1)];
    let mean = v.iter().sum::<u64>() / n as u64;
    println!(
        "{label:28} n={n:<3} min={:<5} p50={:<5} p90={:<5} max={:<5} mean={mean}ms",
        v[0],
        pct(50),
        pct(90),
        v[n - 1],
    );
}

/// One consensus-free round-trip to each guardian (liveness) and to the gateway
/// (info), recorded in microseconds. Both handlers answer immediately, so the
/// elapsed time is ~network RTT plus trivial server handling over the same warm
/// pooled connections the timed ops use. Failed probes are skipped, not fatal.
async fn sample_rtt(
    client: &Client,
    peers: &[PeerId],
    gw: GatewayPk,
    guardian_rtt: &mut BTreeMap<PeerId, Vec<u64>>,
    gateway_rtt: &mut Vec<u64>,
) {
    for &peer in peers {
        let t = Instant::now();
        if client.api().liveness_peer(peer).await.is_ok() {
            guardian_rtt
                .entry(peer)
                .or_default()
                .push(t.elapsed().as_micros() as u64);
        }
    }

    let t = Instant::now();
    if client.ln().ping_gateway(gw).await.is_ok() {
        gateway_rtt.push(t.elapsed().as_micros() as u64);
    }
}

/// Like [`report`] but for RTT samples held in microseconds, printed in
/// milliseconds with sub-millisecond precision.
fn report_rtt(label: &str, v: &mut [u64]) {
    if v.is_empty() {
        return;
    }
    v.sort_unstable();
    let n = v.len();
    let ms = |x: u64| x as f64 / 1000.0;
    let pct = |p: usize| ms(v[(n * p / 100).min(n - 1)]);
    let mean = ms(v.iter().sum::<u64>() / n as u64);
    println!(
        "{label:28} n={n:<3} min={:<7.2} p50={:<7.2} p90={:<7.2} max={:<7.2} mean={mean:.2}ms",
        ms(v[0]),
        pct(50),
        pct(90),
        ms(v[n - 1]),
    );
}

async fn wait_mint_success(client: &Client, op: OperationId) -> anyhow::Result<()> {
    let mut stream = client.subscribe_operation_events(op);
    while let Some(entry) = stream.next().await {
        if entry.to_event::<MintSuccessEvent>().is_some() {
            return Ok(());
        }
        if let Some(ev) = entry.to_event::<TxRejectEvent>() {
            anyhow::bail!("tx rejected: {}", ev.error);
        }
    }
    anyhow::bail!("operation event stream ended")
}

async fn wait_ln_send(client: &Client, op: OperationId) -> anyhow::Result<()> {
    let mut stream = client.subscribe_operation_events(op);
    while let Some(entry) = stream.next().await {
        if entry.to_event::<SendSuccessEvent>().is_some() {
            return Ok(());
        }
        if let Some(ev) = entry.to_event::<SendRefundEvent>() {
            anyhow::bail!("ln send refunded (expired={})", ev.expired);
        }
    }
    anyhow::bail!("operation event stream ended")
}

/// Tail the global event log from `from` and return once the next ln
/// `ReceiveEvent` lands (the circular self-pay's receive completion).
async fn wait_ln_receive(client: &Client, from: EventLogId) -> anyhow::Result<()> {
    let notify = client.event_notify();
    let mut next = from;
    loop {
        let notified = notify.notified();
        let batch = client.get_event_log(next, 100).await;
        for (id, entry) in batch {
            next = id.saturating_add(1);
            if entry.to_event::<LnReceiveEvent>().is_some() {
                return Ok(());
            }
        }
        notified.await;
    }
}

async fn log_end(client: &Client) -> EventLogId {
    let mut pos = EventLogId::LOG_START;
    loop {
        let batch = client.get_event_log(pos, 1000).await;
        let Some((last, _)) = batch.last() else {
            return pos;
        };
        pos = last.saturating_add(1);
    }
}
