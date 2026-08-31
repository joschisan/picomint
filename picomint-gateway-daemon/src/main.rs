#![warn(missing_docs)]
//! This crate provides the Picomint gateway binary.
//!
//! The binary contains logic for sending/receiving Lightning payments on behalf
//! of Picomint clients in one or more connected Federations.
//!
//! It runs a webserver with a REST API that can be used by Picomint
//! clients to request routing of payments through the Lightning Network.
//! The API also has endpoints for managing the gateway.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, ensure};
use bitcoin::Network;
use clap::{ArgGroup, Parser};
use iroh::endpoint::presets::N0;
use iroh_mdns_address_lookup::MdnsAddressLookup;
use lightning::types::payment::PaymentHash;
use picomint_core::Amount;
use picomint_core::core::OperationId;
use picomint_core::ln::gateway::PaymentFee;
use picomint_gateway_daemon::db::{
    IncomingOfferTable, OutgoingContractTable, ProcessedLdkEventTable,
};
use picomint_gateway_daemon::{AppState, DB_FILE, LDK_NODE_DB_FOLDER, cli, connect, public};
use picomint_sqlite::{DbRead, WriteTx};
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
    #[arg(long = "network", env = "BITCOIN_NETWORK", default_value = "bitcoin")]
    pub network: Network,

    /// Esplora HTTP base URL, e.g. <https://mempool.space/api>
    #[arg(long, env = "ESPLORA_URL")]
    pub esplora_url: Option<Url>,

    /// Bitcoind RPC URL with embedded credentials, e.g.
    /// `http://user:pass@127.0.0.1:8332`.
    #[arg(long, env = "BITCOIND_URL")]
    pub bitcoind_url: Option<Url>,

    /// Public API listen address. The iroh endpoint binds here for the
    /// gateway-API and outgoing federation client traffic.
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
    pub send_fee_ppm: u64,

    /// Base receive fee in millisatoshis: the gateway's tx cut on incoming payments.
    #[arg(long, env = "RECEIVE_FEE_BASE_MSAT", default_value_t = 10_000)]
    pub receive_fee_base_msat: u64,

    /// Receive fee rate in parts per million: the gateway's tx cut on incoming payments.
    #[arg(long, env = "RECEIVE_FEE_PPM", default_value_t = 1000)]
    pub receive_fee_ppm: u64,

    /// BOLT11 invoice expiry, in seconds, for invoices the gateway issues.
    #[arg(long, env = "INVOICE_EXPIRY_SECS", default_value_t = 86_400)]
    pub invoice_expiry_secs: u32,

    /// Maximum total CLTV expiry delta, in blocks, the gateway will accept
    /// across the outgoing route. Used as LDK's `max_total_cltv_expiry_delta`
    /// and (with a +144-block safety slack) as the contract lock the gateway
    /// announces to clients.
    #[arg(long, env = "CLTV_EXPIRY_DELTA", default_value_t = 500)]
    pub cltv_expiry_delta: u32,
}

fn main() -> anyhow::Result<()> {
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
        base: Amount::from_msat(opts.send_fee_base_msat),
        ppm: opts.send_fee_ppm,
    };

    ensure!(
        send_fee.is_within(&PaymentFee::SEND_FEE_LIMIT),
        "Configured send fee {send_fee:?} exceeds the limit clients accept {:?}",
        PaymentFee::SEND_FEE_LIMIT,
    );

    let receive_fee = PaymentFee {
        base: Amount::from_msat(opts.receive_fee_base_msat),
        ppm: opts.receive_fee_ppm,
    };

    ensure!(
        receive_fee.is_within(&PaymentFee::RECEIVE_FEE_LIMIT),
        "Configured receive fee {receive_fee:?} exceeds the limit clients accept {:?}",
        PaymentFee::RECEIVE_FEE_LIMIT,
    );

    let runtime = Arc::new(tokio::runtime::Runtime::new()?);

    // 2. Open database
    tokio_rustls::rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let gateway_db = picomint_sqlite::Database::open(opts.data_dir.join(DB_FILE))?;

    // 3. Load or init the gateway identity: the mnemonic (federation-client
    // seed + LDK entropy) and the independent iroh key the public API is
    // served under.
    let mnemonic = picomint_gateway_daemon::db::load_or_init_mnemonic(&gateway_db)?;

    let iroh_secret_key = picomint_gateway_daemon::db::load_or_init_iroh_secret_key(&gateway_db);

    let endpoint = runtime.block_on(
        iroh::Endpoint::builder(N0)
            .secret_key(iroh_secret_key)
            .alpns(vec![picomint_rpc::ALPN.to_vec()])
            .bind_addr(opts.api_addr)?
            .address_lookup(MdnsAddressLookup::builder())
            .bind(),
    )?;

    let client = Arc::new(picomint_client::Client::new(
        endpoint.clone(),
        gateway_db.clone(),
        mnemonic.clone(),
        None,
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
        analytics: picomint_gateway_daemon::analytics::Analytics::wipe_and_init(&opts.data_dir)?,
    };

    // 6. Fire-and-forget every long-running task. Federation clients are
    //    lazy-loaded on first use; all work is persisted incrementally and
    //    idempotent on retry, so the runtime drop on process exit aborts
    //    cleanly.
    runtime.spawn(public::run_public(state.clone(), endpoint.clone()));

    runtime.spawn(cli::run_cli(state.clone()));

    runtime.spawn(process_ldk_events(state.clone()));

    runtime.spawn(picomint_gateway_daemon::analytics::trailer(state.clone()));

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
        } => handle_payment_claimable(state, &dbtx, payment_hash.0, claimable_amount_msat),
        ldk_node::Event::PaymentSuccessful {
            payment_hash,
            payment_preimage: Some(preimage),
            fee_paid_msat,
            ..
        } => handle_payment_successful(
            state,
            &dbtx,
            payment_hash.0,
            preimage.0,
            Amount::from_msat(fee_paid_msat.unwrap_or(0)),
        ),
        ldk_node::Event::PaymentFailed {
            payment_hash: Some(ph),
            ..
        } => handle_payment_failed(state, &dbtx, ph.0),
        _ => return,
    }

    dbtx.commit();
}

/// Inbound HTLC arrived. Fund the registered offer via `start_receive`.
/// On amount mismatch or `start_receive` failure (e.g. insufficient
/// gateway liquidity to fund the contract), log the reason and fail the
/// HTLC so the LN sender gets a refund.
fn handle_payment_claimable(
    state: &AppState,
    dbtx: &WriteTx,
    payment_hash: [u8; 32],
    amount_msat: u64,
) {
    let operation = OperationId::from_encodable(&payment_hash);

    if dbtx
        .insert(&ProcessedLdkEventTable, &payment_hash, &())
        .is_some()
    {
        return;
    }

    // LDK only fires PaymentClaimable for hashes we registered via
    // `receive_for_hash` in `AppState::receive`, which commits the
    // offer row before returning the invoice.
    let row = dbtx
        .get(&IncomingOfferTable, &operation)
        .expect("PaymentClaimable for an unregistered payment_hash");

    if row.offer.commitment.amount.msat != amount_msat {
        state
            .node
            .bolt11_payment()
            .fail_for_hash(PaymentHash(payment_hash))
            .expect("LDK has this payment_hash (registered via receive_for_hash)");
    } else {
        if state
            .client
            .gw_start_receive(row.federation, dbtx, operation, row.offer)
            .is_err()
        {
            state
                .node
                .bolt11_payment()
                .fail_for_hash(PaymentHash(payment_hash))
                .expect("LDK has this payment_hash (registered via receive_for_hash)");
        }
    }
}

/// Outbound LN payment succeeded. Look up the outgoing contract row and
/// tell the source federation's client to finalize the send with the
/// preimage carried on the `PaymentSuccessful` event.
fn handle_payment_successful(
    state: &AppState,
    dbtx: &WriteTx,
    payment_hash: [u8; 32],
    preimage: [u8; 32],
    ln_fee: Amount,
) {
    let operation = OperationId::from_encodable(&payment_hash);

    if dbtx
        .insert(&ProcessedLdkEventTable, &payment_hash, &())
        .is_some()
    {
        return;
    }

    if let Some(row) = dbtx.get(&OutgoingContractTable, &operation) {
        state
            .client
            .gw_finalize_send(
                row.federation,
                dbtx,
                operation,
                row.contract,
                row.outpoint,
                Some((preimage, ln_fee)),
            )
            .expect("source federation for outgoing contract is joined");
    }
}

/// Outbound LN payment failed. Look up the outgoing contract row and tell
/// the source federation's client to forfeit the contract.
fn handle_payment_failed(state: &AppState, dbtx: &WriteTx, payment_hash: [u8; 32]) {
    let operation = OperationId::from_encodable(&payment_hash);

    if dbtx
        .insert(&ProcessedLdkEventTable, &payment_hash, &())
        .is_some()
    {
        return;
    }

    if let Some(row) = dbtx.get(&OutgoingContractTable, &operation) {
        state
            .client
            .gw_finalize_send(
                row.federation,
                dbtx,
                operation,
                row.contract,
                row.outpoint,
                None,
            )
            .expect("source federation for outgoing contract is joined");
    }
}
