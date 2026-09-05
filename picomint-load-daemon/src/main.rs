//! Synthetic load for a mint: one self-payment through the mint's gateway
//! every `INTERVAL_SECS`. Each payment is an invoice the client requests
//! from the gateway and then pays back through the same gateway, which the
//! gateway settles as an internal swap — two mint transactions per round,
//! one incoming and one outgoing, so its analytics fill on a steady cadence
//! without any Lightning channel.
//!
//! The wallet persists in `DATA_DIR`; on first start it prints a deposit
//! address and waits until someone pegs funds in.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::Parser;
use futures::StreamExt as _;
use iroh::Endpoint;
use iroh::endpoint::presets::N0;
use iroh_mdns_address_lookup::MdnsAddressLookup;
use picomint_client::lightning::events::{SendRefundEvent, SendSuccessEvent};
use picomint_client::{Account, Client, Mnemonic, random_mnemonic};
use picomint_core::Amount;
use picomint_core::config::MintId;
use picomint_core::invite::InviteCode;
use picomint_redb::{Database, DbRead, table};
use rand::rngs::OsRng;
use tokio::time::{MissedTickBehavior, interval, sleep, timeout};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::util::TryInitError;

const DB_FILE: &str = "database.redb";

/// A payment that has not settled within this window is abandoned by the
/// loop (its state machine keeps running in the client) so one stuck
/// round cannot stall the cadence for good.
const PAYMENT_TIMEOUT: Duration = Duration::from_secs(120);

const FUNDING_POLL: Duration = Duration::from_secs(10);

table!(
    RootEntropyTable,
    () => Vec<u8>,
    "load-root-entropy",
);

#[derive(Debug, Parser)]
struct CliOpts {
    /// Directory holding the wallet database.
    #[arg(long, env = "DATA_DIR")]
    data_dir: PathBuf,
    /// Invite code of the mint to load. Read on first start only; the mint
    /// is persisted in the database after that.
    #[arg(long, env = "INVITE")]
    invite: InviteCode,
    /// Bitcoin network the mint must be on.
    #[arg(long, env = "NETWORK")]
    network: bitcoin::Network,
    /// Seconds between the start of one payment and the next.
    #[arg(long, env = "INTERVAL_SECS", default_value_t = 10)]
    interval_secs: u64,
    /// Amount of each self-payment.
    #[arg(long, env = "AMOUNT_SAT", default_value_t = 1_000)]
    amount_sat: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing()?;

    let opts = CliOpts::parse();

    let db = Database::open(opts.data_dir.join(DB_FILE))?;

    let mnemonic = load_or_init_mnemonic(&db)?;

    // No secret key: nothing dials a wallet, so its network identity is
    // ephemeral.
    let endpoint = Endpoint::builder(N0)
        .transport_config(picomint_rpc::transport_config())
        .address_lookup(MdnsAddressLookup::builder())
        .bind()
        .await?;

    let client = Arc::new(Client::new(endpoint, db, mnemonic));

    let mint = match client.mints().first() {
        Some(mint) => *mint,
        None => client.add_mint(&opts.invite, Some(opts.network)).await?,
    };

    info!(%mint, "load-daemon started");

    let amount = Amount::from_sat(opts.amount_sat);

    await_funding(&client, mint, amount).await?;

    await_gateway(&client, mint).await;

    let mut ticker = interval(Duration::from_secs(opts.interval_secs));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                match timeout(PAYMENT_TIMEOUT, self_pay(&client, mint, amount)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => warn!(error = %e, "payment failed"),
                    Err(_) => warn!("payment did not settle within {PAYMENT_TIMEOUT:?}, moving on"),
                }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }

    info!("shutting down");

    client.shutdown().await;

    Ok(())
}

fn init_tracing() -> Result<(), TryInitError> {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .try_init()
}

fn load_or_init_mnemonic(db: &Database) -> anyhow::Result<Mnemonic> {
    if let Some(entropy) = db.begin_read().get(&RootEntropyTable, &()) {
        return Mnemonic::from_entropy(&entropy).context("invalid stored entropy");
    }

    let mnemonic = random_mnemonic(&mut OsRng);

    let dbtx = db.begin_write();

    dbtx.insert(&RootEntropyTable, &(), &mnemonic.to_entropy());

    dbtx.commit();

    Ok(mnemonic)
}

/// Block until the wallet holds at least two payments' worth, printing the
/// deposit address so an operator can peg in. The address derivation
/// itself lands via consensus shortly after the mint is added.
async fn await_funding(client: &Client, mint: MintId, amount: Amount) -> anyhow::Result<()> {
    let required = amount.mul_u64(2);

    loop {
        let balance = client.ecash_balance(mint, Account::Primary);

        if balance >= required {
            info!(%balance, "wallet funded");
            return Ok(());
        }

        match client.onchain_receive(mint, Account::Primary) {
            Ok(address) => info!(%balance, %required, %address, "waiting for funding"),
            Err(e) => info!(error = %e, "waiting for deposit address"),
        }

        sleep(FUNDING_POLL).await;
    }
}

async fn await_gateway(client: &Client, mint: MintId) {
    while client.lightning_select_gateway(mint).is_err() {
        if let Err(e) = client.lightning_refresh_gateways(mint).await {
            warn!(error = %e, "gateway refresh failed");
        }

        sleep(FUNDING_POLL).await;
    }
}

async fn self_pay(client: &Client, mint: MintId, amount: Amount) -> anyhow::Result<()> {
    let (gateway_pk, gateway_info) = client.lightning_select_gateway(mint)?;

    let started = Instant::now();

    let invoice = client
        .lightning_receive(
            mint,
            Account::Primary,
            gateway_pk,
            gateway_info.clone(),
            amount,
        )
        .await?;

    let invoice_ms = started.elapsed().as_millis() as u64;

    let operation = client
        .lightning_send(mint, Account::Primary, gateway_pk, gateway_info, invoice)
        .await?;

    let mut events = client.subscribe_operation_events(operation);

    while let Some(entry) = events.next().await {
        if entry.to_event::<SendSuccessEvent>().is_some() {
            info!(
                %operation,
                invoice_ms,
                total_ms = started.elapsed().as_millis() as u64,
                balance = %client.ecash_balance(mint, Account::Primary),
                "payment settled"
            );
            return Ok(());
        }

        if let Some(e) = entry.to_event::<SendRefundEvent>() {
            bail!("payment refunded (expired = {})", e.expired);
        }
    }

    bail!("event stream ended before the payment settled")
}
