#![warn(missing_docs)]
//! This crate provides the Picomint gateway binary.
//!
//! The binary contains logic for sending/receiving Lightning payments on behalf
//! of Picomint clients in one or more connected Mints.
//!
//! It serves Picomint clients over a public iroh endpoint (consensus-encoded
//! `GatewayMethod` requests) to route payments through the Lightning
//! Network, and is managed through a separate HTTP-over-Unix-socket admin
//! CLI.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, ensure};
use bitcoin::Network;
use bitcoin::hashes::{Hash, sha256};
use clap::{ArgGroup, Parser};
use iroh::endpoint::presets::N0;
use iroh_mdns_address_lookup::MdnsAddressLookup;
use lightning::types::payment::{PaymentHash, PaymentPreimage};
use picomint_core::Amount;
use picomint_core::core::OperationId;
use picomint_core::lightning::gateway::PaymentFee;
use picomint_gateway_daemon::db::{Payment, PaymentTable};
use picomint_gateway_daemon::{AppState, DB_FILE, LDK_NODE_DB_FOLDER, cli, connect, public};
use picomint_redb::{DbRead, WriteTx};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use url::Url;

/// The LDK project's Rapid Gossip Sync server; serves mainnet snapshots only.
const RGS_SERVER_URL: &str = "https://rapidsync.lightningdevkit.org/snapshot";

/// Command line parameters for starting the gateway.
#[derive(Parser)]
#[command(version)]
#[command(
    group(
        ArgGroup::new("bitcoin_rpc")
            .required(true)
            .multiple(true)
            .args(["bitcoind_url", "esplora_url"])
    )
)]
pub struct GatewayOpts {
    /// Path to folder containing gateway config and data files
    #[arg(long = "data-dir", env = "DATA_DIR")]
    pub data_dir: PathBuf,

    /// Bitcoin network this gateway will be running on
    #[arg(long = "network", env = "NETWORK")]
    pub network: Network,

    /// Esplora HTTP base URL, e.g. <https://mempool.space/api>
    #[arg(long, env = "ESPLORA_URL")]
    pub esplora_url: Option<Url>,

    /// Bitcoind RPC URL with embedded credentials, e.g.
    /// `http://user:pass@127.0.0.1:8332`.
    #[arg(long, env = "BITCOIND_URL")]
    pub bitcoind_url: Option<Url>,

    /// Public API listen address. The iroh endpoint binds here for the
    /// gateway-API and outgoing mint client traffic.
    #[arg(long = "api-addr", env = "API_ADDR", default_value = "0.0.0.0:8080")]
    pub api_addr: SocketAddr,

    /// Network address and port for the lightning P2P interface (BOLT)
    #[arg(long = "ldk-addr", env = "LDK_ADDR", default_value = "0.0.0.0:9735")]
    pub ldk_addr: SocketAddr,

    /// Base send fee in millisatoshis: the gateway's tx cut on outgoing payments.
    #[arg(long, env = "SEND_FEE_BASE_MSAT", default_value_t = 10_000)]
    pub send_fee_base_msat: u64,

    /// Send fee rate in parts per million: the gateway's tx cut on outgoing payments.
    #[arg(long, env = "SEND_FEE_PPM", default_value_t = 3000)]
    pub send_fee_ppm: u16,

    /// Base receive fee in millisatoshis: the gateway's tx cut on incoming payments.
    #[arg(long, env = "RECEIVE_FEE_BASE_MSAT", default_value_t = 10_000)]
    pub receive_fee_base_msat: u64,

    /// Receive fee rate in parts per million: the gateway's tx cut on incoming payments.
    #[arg(long, env = "RECEIVE_FEE_PPM", default_value_t = 1000)]
    pub receive_fee_ppm: u16,

    /// BOLT11 invoice expiry, in seconds, for invoices the gateway issues.
    #[arg(long, env = "INVOICE_EXPIRY_SECS", default_value_t = 86_400)]
    pub invoice_expiry_secs: u32,

    /// Maximum total CLTV expiry delta, in blocks, the gateway will accept
    /// across the outgoing route, as LDK's `max_total_cltv_expiry_delta`.
    #[arg(long, env = "CLTV_EXPIRY_DELTA", default_value_t = 500)]
    pub cltv_expiry_delta: u32,
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

    // 1. Parse CLI args
    let opts = GatewayOpts::parse();

    // Clients enforce these limits on every payment, so a gateway
    // configured above them would be rejected by every client; fail
    // fast at startup instead.
    let send_fee = PaymentFee {
        base: Amount(opts.send_fee_base_msat),
        ppm: opts.send_fee_ppm,
    };

    ensure!(
        send_fee.is_within(&PaymentFee::SEND_FEE_LIMIT),
        "Configured send fee {send_fee:?} exceeds the limit clients accept {:?}",
        PaymentFee::SEND_FEE_LIMIT,
    );

    let receive_fee = PaymentFee {
        base: Amount(opts.receive_fee_base_msat),
        ppm: opts.receive_fee_ppm,
    };

    ensure!(
        receive_fee.is_within(&PaymentFee::RECEIVE_FEE_LIMIT),
        "Configured receive fee {receive_fee:?} exceeds the limit clients accept {:?}",
        PaymentFee::RECEIVE_FEE_LIMIT,
    );

    // 2. Open database
    let gateway_db = picomint_redb::Database::open(opts.data_dir.join(DB_FILE))?;

    // 3. Load or init the gateway identity: the mnemonic (mint-client
    // seed + LDK entropy) and the independent iroh key the public API is
    // served under.
    let mnemonic = picomint_gateway_daemon::db::load_or_init_mnemonic(&gateway_db)?;

    let iroh_secret_key = picomint_gateway_daemon::db::load_or_init_iroh_secret_key(&gateway_db);

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

    // `Client::new` brings every added mint up, which spawns their
    // background tasks — so the runtime stays entered from here on.
    let _rt = runtime.enter();

    let client = Arc::new(picomint_client::Client::new_gateway(
        endpoint.clone(),
        gateway_db.clone(),
        mnemonic.clone(),
    ));

    // 4. Build LDK node
    let ldk_data_dir = opts
        .data_dir
        .join(LDK_NODE_DB_FOLDER)
        .to_str()
        .expect("Invalid data dir path")
        .to_string();

    let mut node_builder = ldk_node::Builder::new();

    node_builder.set_runtime(runtime.handle().clone());
    node_builder.set_network(opts.network);
    node_builder.set_node_alias("picomint-gateway-daemon".to_string())?;
    node_builder.set_listening_addresses(vec![opts.ldk_addr.into()])?;
    node_builder.set_entropy_bip39_mnemonic(mnemonic.clone(), None);
    node_builder.set_storage_dir_path(ldk_data_dir);

    // The default peer-to-peer gossip sync takes hours to assemble a usable
    // network graph on a fresh node, and every routed payment fails with
    // `RouteNotFound` until it does — pull snapshots from the LDK project's
    // Rapid Gossip Sync server instead. There is no RGS server for regtest,
    // where the two-node test topology needs no gossip anyway.
    if opts.network == Network::Bitcoin {
        node_builder.set_gossip_source_rgs(RGS_SERVER_URL.to_string());
    }

    match (opts.bitcoind_url.clone(), opts.esplora_url.clone()) {
        (Some(url), _) => {
            let host = url
                .host_str()
                .context("BITCOIND_URL is missing a host")?
                .to_string();

            let port = url.port().context("BITCOIND_URL is missing a port")?;

            let username = url.username().to_owned();

            let password = url
                .password()
                .context("BITCOIND_URL must embed credentials: http://user:pass@host")?
                .to_owned();

            node_builder.set_chain_source_bitcoind_rpc(host, port, username, password);
        }
        (None, Some(url)) => {
            node_builder.set_chain_source_esplora(url.to_string(), None);
        }
        _ => unreachable!("ArgGroup enforces at least one chain source"),
    }

    info!("Starting LDK Node...");

    let node = Arc::new(node_builder.build()?);

    node.start()?;

    info!("Successfully started LDK Node");

    // On a fresh node with no persisted peers yet, seed connections to a few
    // large, well-run public nodes so the node participates in the network
    // right away.
    if opts.network == Network::Bitcoin && node.list_peers().is_empty() {
        for &(name, node_id, address) in connect::PUBLIC_NODES {
            let node_id = node_id.parse().expect("node id is valid");
            let address = address.parse().expect("address is valid");

            if let Err(err) = node.connect(node_id, address, true) {
                warn!(%err, name, "Failed to connect to public node");
            }
        }
    }

    // 5. Construct AppState
    let state = AppState {
        client,
        endpoint: endpoint.clone(),
        mnemonic,
        node: node.clone(),
        gateway_db,
        data_dir: opts.data_dir.clone(),
        network: opts.network,
        send_fee,
        receive_fee,
        invoice_expiry_secs: opts.invoice_expiry_secs,
        cltv_expiry_delta: opts.cltv_expiry_delta,
        analytics: picomint_analytics::Analytics::wipe_and_init(&opts.data_dir)?,
    };

    // 6. Fire-and-forget every long-running task. All work is persisted
    //    incrementally and idempotent on retry, so the runtime drop on
    //    process exit aborts cleanly.
    runtime.spawn(public::run(state.clone(), endpoint.clone()));

    runtime.spawn(cli::run(state.clone())?);

    runtime.spawn(process_ldk_events(state.clone()));

    runtime.spawn(picomint_analytics::trailer(
        state.client.clone(),
        state.analytics.clone(),
    ));

    runtime.spawn(picomint_gateway_daemon::trailer::run(state));

    // 7. Block main on SIGTERM so the runtime stays alive; on signal,
    //    return Ok and let the runtime drop abort all tasks.
    runtime.block_on(shutdown_signal());

    info!("Gatewayd exiting...");

    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("Failed to install SIGTERM handler")
        .recv()
        .await;
}

// ---------------------------------------------------------------------------
// LDK event loop
// ---------------------------------------------------------------------------

async fn process_ldk_events(state: AppState) {
    loop {
        let event = state.node.next_event_async().await;

        process_ldk_event(&state, event);

        state
            .node
            .event_handled()
            .expect("LDK event_handled persistence failed");
    }
}

fn process_ldk_event(state: &AppState, event: ldk_node::Event) {
    let dbtx = state.gateway_db.begin_write();

    match event {
        ldk_node::Event::PaymentClaimable {
            payment_hash,
            claimable_amount_msat,
            ..
        } => handle_payment_claimable(
            state,
            &dbtx,
            sha256::Hash::from_byte_array(payment_hash.0),
            claimable_amount_msat,
        ),
        ldk_node::Event::PaymentSuccessful {
            payment_hash,
            payment_preimage: Some(preimage),
            fee_paid_msat,
            ..
        } => handle_payment_outcome(
            state,
            &dbtx,
            sha256::Hash::from_byte_array(payment_hash.0),
            Some((preimage.0, Amount(fee_paid_msat.unwrap_or(0)))),
        ),
        ldk_node::Event::PaymentFailed {
            payment_hash: Some(ph),
            reason,
            ..
        } => {
            warn!(?reason, payment_hash = ?ph, "The outgoing payment failed; cancelling it");

            handle_payment_outcome(state, &dbtx, sha256::Hash::from_byte_array(ph.0), None)
        }
        _ => return,
    }

    dbtx.commit();
}

/// Inbound HTLC arrived. Fund the contract registered under its hash and
/// settle the HTLC with the preimage in the same step. A hash with no
/// registered contract (one already funded, or wiped with its mint), an
/// amount mismatch or a funding failure (e.g. insufficient gateway
/// liquidity) fails the HTLC so the LN sender gets a refund; so does a
/// replay of a payment already funded, whose claim LDK is already making.
fn handle_payment_claimable(
    state: &AppState,
    dbtx: &WriteTx,
    payment_hash: sha256::Hash,
    amount_msat: u64,
) {
    let Some(Payment::Registered { mint, contract }) = dbtx.get(&PaymentTable, &payment_hash)
    else {
        warn!(%payment_hash, "Failing an inbound HTLC with no registered contract");

        fail_htlc(state, payment_hash);

        return;
    };

    let preimage = contract.preimage();

    if contract.amount.0 != amount_msat
        || state
            .client
            .gateway_start_receive(mint, dbtx, OperationId(payment_hash), contract)
            .is_err()
    {
        fail_htlc(state, payment_hash);

        return;
    }

    dbtx.insert(&PaymentTable, &payment_hash, &Payment::Received);

    // The preimage is the contract's own hash, so the HTLC settles now
    // rather than once the mint accepts the funding, and LDK's fail-back
    // deadline constrains nothing. The funding leaves for the mint on the
    // commit below; a crash before it replays this event, whose repeated
    // claim LDK refuses, which is why a refusal is not fatal.
    if let Err(error) = state.node.bolt11_payment().claim_for_hash(
        PaymentHash(payment_hash.to_byte_array()),
        amount_msat,
        PaymentPreimage(preimage),
    ) {
        warn!(%error, "LDK refused the claim of an inbound HTLC");
    }
}

fn fail_htlc(state: &AppState, payment_hash: sha256::Hash) {
    state
        .node
        .bolt11_payment()
        .fail_for_hash(PaymentHash(payment_hash.to_byte_array()))
        .expect("LDK has this payment_hash (registered via receive_for_hash)");
}

/// Outbound LN payment succeeded or failed. Settle the send its hash is
/// used for, with the preimage the success carries or with the forfeit
/// signature; a hash no send is paying, as for a payment the operator made
/// through the CLI or an event replayed after it settled, is left alone.
fn handle_payment_outcome(
    state: &AppState,
    dbtx: &WriteTx,
    payment_hash: sha256::Hash,
    success: Option<([u8; 32], Amount)>,
) {
    if let Some(Payment::Sending { operation }) = dbtx.get(&PaymentTable, &payment_hash) {
        state.settle_send(dbtx, payment_hash, operation, success);
    }
}
