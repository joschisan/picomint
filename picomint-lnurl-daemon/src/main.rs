use std::net::SocketAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, ensure};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use bitcoin::hashes::sha256;
use bitcoin::secp256k1::{self, Keypair, PublicKey, ecdh};
use clap::Parser;
use iroh::Endpoint;
use iroh::endpoint::presets::N0;
use iroh_mdns_address_lookup::MdnsAddressLookup;
use lightning_invoice::Bolt11Invoice;
use picomint_core::Amount;
use picomint_core::config::FederationId;
use picomint_core::ln::MINIMUM_INCOMING_CONTRACT_AMOUNT;
use picomint_core::ln::contracts::IncomingContract;
use picomint_core::ln::gateway_api::{
    CreateInvoiceRequest, CreateInvoiceResponse, GatewayInfo, GatewayMethod, GatewayPk,
    InfoRequest, InfoResponse, PaymentFee, VerifyPreimageRequest,
    VerifyResponse as GatewayVerifyResponse,
};
use picomint_core::ln::lnurl::{LnurlRequest, MAX_GATEWAYS_PER_LNURL};
use picomint_core::ln::secret::IncomingContractSecret;
use picomint_encoding::{Decodable, Encodable};
use picomint_lnurl::{
    InvoiceResponse, LnurlResponse, PayResponse, VerifyResponse, pay_request_tag,
};
use serde::Deserialize;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tower_http::cors;
use tower_http::cors::CorsLayer;
use tpe::AggregatePublicKey;
use tracing::info;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const MAX_SENDABLE_MSAT: u64 = 100_000_000_000;
const MIN_SENDABLE_MSAT: u64 = 100_000;

#[derive(Debug, Parser)]
struct CliOpts {
    /// Public HTTP API listen address. Should be open in the firewall —
    /// wallets and the paying side hit this directly.
    #[arg(long, env = "API_ADDR", default_value = "0.0.0.0:8080")]
    api_addr: SocketAddr,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .try_init()?;

    let cli_opts = CliOpts::parse();

    let endpoint = Endpoint::builder(N0)
        .address_lookup(MdnsAddressLookup::builder())
        .bind()
        .await?;

    let cors = CorsLayer::new()
        .allow_origin(cors::Any)
        .allow_methods(cors::Any)
        .allow_headers(cors::Any);

    let app = Router::new()
        .route("/", get(health_check))
        .route("/pay/{payload}", get(pay))
        .route("/invoice/{payload}", get(invoice))
        .route("/verify/{gateway_pk}/{payment_hash}", get(verify))
        .layer(cors)
        .with_state(endpoint);

    info!(api_addr = %cli_opts.api_addr, "lnurl-daemon started");

    let listener = TcpListener::bind(cli_opts.api_addr).await?;

    axum::serve(listener, app).await?;

    Ok(())
}

async fn health_check(headers: HeaderMap) -> impl IntoResponse {
    format!("lnurl-daemon is up and running at {}", base_url(&headers))
}

fn base_url(headers: &HeaderMap) -> String {
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost");

    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("http");

    format!("{scheme}://{host}/")
}

async fn pay(headers: HeaderMap, Path(payload): Path<String>) -> Json<LnurlResponse<PayResponse>> {
    Json(LnurlResponse::Ok(PayResponse {
        callback: format!("{}invoice/{payload}", base_url(&headers)),
        max_sendable: MAX_SENDABLE_MSAT,
        min_sendable: MIN_SENDABLE_MSAT,
        tag: pay_request_tag(),
        metadata: "[[\"text/plain\", \"Pay to LNURL daemon\"]]".to_string(),
    }))
}

#[derive(Debug, Serialize, Deserialize)]
struct GetInvoiceParams {
    amount: u64,
}

async fn invoice(
    headers: HeaderMap,
    State(endpoint): State<Endpoint>,
    Path(payload): Path<String>,
    Query(params): Query<GetInvoiceParams>,
) -> Json<LnurlResponse<InvoiceResponse>> {
    let Ok(request) = picomint_base32::decode::<LnurlRequest>(&payload) else {
        return Json(LnurlResponse::error("Failed to decode payload"));
    };

    if request.gateways.len() > MAX_GATEWAYS_PER_LNURL {
        return Json(LnurlResponse::error(format!(
            "Too many gateways in request (max {MAX_GATEWAYS_PER_LNURL})"
        )));
    }

    if params.amount < MIN_SENDABLE_MSAT || params.amount > MAX_SENDABLE_MSAT {
        return Json(LnurlResponse::error(format!(
            "Amount must be between {} and {}",
            MIN_SENDABLE_MSAT, MAX_SENDABLE_MSAT
        )));
    }

    let (gateway_pk, invoice) = match create_contract_and_fetch_invoice(
        &endpoint,
        request.federation,
        request.recipient_pk,
        request.aggregate_pk,
        request.gateways,
        params.amount,
        3600, // standard expiry time of one hour
    )
    .await
    {
        Ok(result) => result,
        Err(e) => {
            return Json(LnurlResponse::error(e.to_string()));
        }
    };

    info!(%params.amount, "Created invoice");

    // The verify URL routes through this daemon: we proxy the call to
    // the originating gateway over iroh. `gateway_pk` is base32-encoded
    // via `picomint_base32` (same format as the rest of picomint).
    let verify = format!(
        "{}verify/{}/{}",
        base_url(&headers),
        picomint_base32::encode(&gateway_pk),
        invoice.payment_hash()
    );

    Json(LnurlResponse::Ok(InvoiceResponse {
        pr: invoice.clone(),
        verify: Some(verify),
    }))
}

async fn create_contract_and_fetch_invoice(
    endpoint: &Endpoint,
    federation: FederationId,
    recipient_pk: PublicKey,
    aggregate_pk: AggregatePublicKey,
    gateways: Vec<GatewayPk>,
    amount: u64,
    expiry_secs: u32,
) -> anyhow::Result<(GatewayPk, Bolt11Invoice)> {
    let ephemeral_keypair = Keypair::new(secp256k1::SECP256K1, &mut rand::thread_rng());

    let shared_secret =
        ecdh::SharedSecret::new(&recipient_pk, &ephemeral_keypair.secret_key()).secret_bytes();

    let contract_secret = IncomingContractSecret::new(shared_secret);

    let encryption_seed = contract_secret.encryption_seed();
    let preimage = contract_secret.preimage();
    let claim_tweak = contract_secret.claim_tweak();

    let claim_pk = recipient_pk
        .mul_tweak(secp256k1::SECP256K1, &claim_tweak)
        .expect("Tweak is valid")
        .x_only_public_key()
        .0;

    let (gateway_info, gateway_pk) = select_gateway(endpoint, gateways, federation).await?;

    ensure!(
        gateway_info.receive_fee.le(&PaymentFee::RECEIVE_FEE_LIMIT),
        "Payment fee exceeds limit"
    );

    let fee = gateway_info.receive_fee.fee(amount);

    ensure!(
        amount
            .checked_sub(fee.msats)
            .is_some_and(|net| Amount::from_msats(net) >= MINIMUM_INCOMING_CONTRACT_AMOUNT),
        "Amount too small"
    );

    let expiration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("System time before Unix epoch")
        .as_secs()
        .saturating_add(u64::from(expiry_secs));

    let contract = IncomingContract::new(
        aggregate_pk,
        encryption_seed,
        preimage,
        preimage.consensus_hash(),
        Amount::from_msats(amount),
        fee,
        expiration,
        claim_pk,
        gateway_info.module_public_key,
        ephemeral_keypair.public_key(),
    );

    let invoice = gateway_request::<CreateInvoiceResponse>(
        endpoint,
        gateway_pk,
        GatewayMethod::CreateInvoice(CreateInvoiceRequest {
            federation,
            contract: contract.clone(),
            amount: Amount::from_msats(amount),
            expiry_secs,
        }),
    )
    .await?
    .invoice;

    ensure!(
        invoice.payment_hash() == &preimage.consensus_hash(),
        "Invalid invoice payment hash"
    );

    ensure!(
        invoice.amount_milli_satoshis() == Some(amount),
        "Invalid invoice amount"
    );

    Ok((gateway_pk, invoice))
}

async fn select_gateway(
    endpoint: &Endpoint,
    gateways: Vec<GatewayPk>,
    federation: FederationId,
) -> anyhow::Result<(GatewayInfo, GatewayPk)> {
    let mut probes = JoinSet::new();

    for gateway_pk in gateways {
        let endpoint = endpoint.clone();
        probes.spawn(async move {
            let response = gateway_request::<InfoResponse>(
                &endpoint,
                gateway_pk,
                GatewayMethod::Info(InfoRequest { federation }),
            )
            .await
            .ok()?;
            Some((response.info?, gateway_pk))
        });
    }

    while let Some(result) = probes.join_next().await {
        if let Ok(Some(hit)) = result {
            return Ok(hit);
        }
    }

    bail!("All gateways are offline or do not support this federation")
}

/// Proxy LUD-21 verify: external LNURL wallet hits us at
/// `/verify/{gateway_pk}/{payment_hash}` (URL embedded in the LNURL pay
/// response), we forward via iroh to the originating gateway. The
/// optional `?wait` query param turns this into a long-poll on the
/// gateway side.
///
/// LUD-21 has no transient-vs-terminal error distinction — a wallet
/// that sees `{"status":"ERROR"}` (or once `settled:true`, later
/// `settled:false`) will give up. So on transport failure we return
/// HTTP 502 with an empty body: the wallet's JSON parse fails the same
/// way as a network error, and any sane polling client retries.
async fn verify(
    State(endpoint): State<Endpoint>,
    Path((gateway_pk, hash)): Path<(GatewayPk, sha256::Hash)>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<LnurlResponse<VerifyResponse>>, StatusCode> {
    let wait = query.contains_key("wait");

    let response = gateway_request::<GatewayVerifyResponse>(
        &endpoint,
        gateway_pk,
        GatewayMethod::VerifyPreimage(VerifyPreimageRequest { hash, wait }),
    )
    .await
    .map_err(|_| StatusCode::BAD_GATEWAY)?;

    Ok(Json(LnurlResponse::Ok(VerifyResponse {
        settled: response.settled,
        preimage: response.preimage,
    })))
}

async fn gateway_request<R: Decodable>(
    endpoint: &Endpoint,
    gateway_pk: GatewayPk,
    method: GatewayMethod,
) -> anyhow::Result<R> {
    picomint_rpc::request(endpoint, gateway_pk.0, method).await
}
