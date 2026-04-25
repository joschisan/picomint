use std::net::SocketAddr;

use anyhow::{anyhow, bail, ensure};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use bitcoin::hashes::sha256;
use bitcoin::secp256k1::{self, Keypair, PublicKey, ecdh};
use clap::Parser;
use iroh::Endpoint;
use iroh::address_lookup::MdnsAddressLookup;
use iroh::endpoint::Connection;
use iroh::endpoint::presets::N0;
use lightning_invoice::Bolt11Invoice;
use picomint_core::Amount;
use picomint_core::config::FederationId;
use picomint_core::ln::contracts::{IncomingContract, PaymentImage};
use picomint_core::ln::gateway_api::{
    CreateBolt11InvoiceRequest, CreateBolt11InvoiceResponse, GATEWAY_MAX_MESSAGE_BYTES,
    GatewayInfo, GatewayInfoRequest, GatewayInfoResponse, GatewayMethod, PaymentFee,
    VerifyBolt11PreimageRequest, VerifyBolt11PreimageResponse,
};
use picomint_core::ln::lnurl::LnurlRequest;
use picomint_core::ln::secret::IncomingContractSecret;
use picomint_core::ln::{Bolt11InvoiceDescription, MINIMUM_INCOMING_CONTRACT_AMOUNT};
use picomint_core::module::ApiError;
use picomint_core::module::PICOMINT_ALPN;
use picomint_core::time::duration_since_epoch;
use picomint_encoding::{Decodable, Encodable};
use picomint_lnurl::{
    InvoiceResponse, LnurlResponse, PayResponse, VerifyResponse, pay_request_tag,
};
use picomint_logging::TracingSetup;
use serde::Deserialize;
use serde::Serialize;
use tokio::net::TcpListener;
use tower_http::cors;
use tower_http::cors::CorsLayer;
use tpe::AggregatePublicKey;
use tracing::info;

const MAX_SENDABLE_MSAT: u64 = 100_000_000_000;
const MIN_SENDABLE_MSAT: u64 = 100_000;

#[derive(Debug, Parser)]
struct CliOpts {
    /// Public HTTP API listen address. Should be open in the firewall —
    /// wallets and the paying side hit this directly.
    #[arg(long, env = "API_ADDR", default_value = "0.0.0.0:8080")]
    api_addr: SocketAddr,
}

/// State shared with request handlers. The iroh endpoint is used
/// client-only (dialing gateways); we don't accept inbound iroh
/// traffic, so we bind to an ephemeral UDP port without publishing it.
#[derive(Clone)]
struct AppState {
    endpoint: Endpoint,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    TracingSetup::default().init()?;

    let cli_opts = CliOpts::parse();

    let endpoint = Endpoint::builder(N0)
        .address_lookup(MdnsAddressLookup::builder())
        .bind()
        .await?;

    let state = AppState { endpoint };

    let cors = CorsLayer::new()
        .allow_origin(cors::Any)
        .allow_methods(cors::Any)
        .allow_headers(cors::Any);

    let app = Router::new()
        .route("/", get(health_check))
        .route("/pay/{payload}", get(pay))
        .route("/invoice/{payload}", get(invoice))
        .route("/verify/{gateway}/{payment_hash}", get(verify))
        .with_state(state)
        .layer(cors);

    info!(api_addr = %cli_opts.api_addr, "recurring-daemon started");

    let listener = TcpListener::bind(cli_opts.api_addr).await?;

    axum::serve(listener, app).await?;

    Ok(())
}

async fn health_check(headers: HeaderMap) -> impl IntoResponse {
    format!(
        "recurring-daemon is up and running at {}",
        base_url(&headers)
    )
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
        metadata: "[[\"text/plain\", \"Pay to Recurringd\"]]".to_string(),
    }))
}

#[derive(Debug, Serialize, Deserialize)]
struct GetInvoiceParams {
    amount: u64,
}

async fn invoice(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(payload): Path<String>,
    Query(params): Query<GetInvoiceParams>,
) -> Json<LnurlResponse<InvoiceResponse>> {
    let Ok(request) = picomint_base32::decode::<LnurlRequest>(&payload) else {
        return Json(LnurlResponse::error("Failed to decode payload"));
    };

    if params.amount < MIN_SENDABLE_MSAT || params.amount > MAX_SENDABLE_MSAT {
        return Json(LnurlResponse::error(format!(
            "Amount must be between {} and {}",
            MIN_SENDABLE_MSAT, MAX_SENDABLE_MSAT
        )));
    }

    let (gateway, invoice) = match create_contract_and_fetch_invoice(
        &state.endpoint,
        request.federation_id,
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

    info!(%params.amount, %gateway, "Created invoice");

    let verify = format!(
        "{}verify/{}/{}",
        base_url(&headers),
        gateway,
        invoice.payment_hash()
    );

    Json(LnurlResponse::Ok(InvoiceResponse {
        pr: invoice.clone(),
        verify: Some(verify),
    }))
}

/// LNURL-pay verify proxy. The gateway lives on iroh now, not HTTP, so
/// recurringd terminates the LNURL `verify` HTTP GET and forwards it
/// to the gateway over iroh. Clients still see an HTTPS URL and get
/// the same JSON envelope back.
async fn verify(
    State(state): State<AppState>,
    Path((gateway, payment_hash)): Path<(String, String)>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Json<LnurlResponse<VerifyResponse>> {
    let Ok(gateway_node_id) = gateway.parse::<iroh::PublicKey>() else {
        return Json(LnurlResponse::error(
            "Invalid gateway node-id in path".to_string(),
        ));
    };

    let Ok(payment_hash) = payment_hash.parse::<sha256::Hash>() else {
        return Json(LnurlResponse::error("Invalid payment hash".to_string()));
    };

    let wait = query.contains_key("wait");

    let connection = match dial(&state.endpoint, gateway_node_id).await {
        Ok(c) => c,
        Err(e) => return Json(LnurlResponse::error(e.to_string())),
    };

    let response: VerifyBolt11PreimageResponse = match gateway_request(
        GatewayMethod::VerifyBolt11Preimage(VerifyBolt11PreimageRequest { payment_hash, wait }),
        &connection,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return Json(LnurlResponse::error(e.to_string())),
    };

    Json(LnurlResponse::Ok(VerifyResponse {
        settled: response.settled,
        preimage: response.preimage,
    }))
}

async fn create_contract_and_fetch_invoice(
    endpoint: &Endpoint,
    federation_id: FederationId,
    recipient_pk: PublicKey,
    aggregate_pk: AggregatePublicKey,
    gateways: Vec<iroh::PublicKey>,
    amount: u64,
    expiry_secs: u32,
) -> anyhow::Result<(iroh::PublicKey, Bolt11Invoice)> {
    let ephemeral_keypair = Keypair::new(secp256k1::SECP256K1, &mut rand::thread_rng());

    let shared_secret =
        ecdh::SharedSecret::new(&recipient_pk, &ephemeral_keypair.secret_key()).secret_bytes();

    let contract_secret = IncomingContractSecret::new(shared_secret);

    let encryption_seed = contract_secret.encryption_seed();
    let preimage = contract_secret.preimage();
    let claim_tweak = contract_secret.claim_tweak();

    let claim_pk = recipient_pk
        .mul_tweak(secp256k1::SECP256K1, &claim_tweak)
        .expect("Tweak is valid");

    let (connection, gateway_info, gateway) =
        select_gateway(endpoint, &gateways, federation_id).await?;

    ensure!(
        gateway_info.receive_fee.le(&PaymentFee::RECEIVE_FEE_LIMIT),
        "Payment fee exceeds limit"
    );

    let contract_amount = gateway_info.receive_fee.subtract_from(amount);

    ensure!(
        contract_amount >= MINIMUM_INCOMING_CONTRACT_AMOUNT,
        "Amount too small"
    );

    let expiration = duration_since_epoch()
        .as_secs()
        .saturating_add(u64::from(expiry_secs));

    let contract = IncomingContract::new(
        aggregate_pk,
        encryption_seed,
        preimage,
        PaymentImage::Hash(preimage.consensus_hash()),
        contract_amount,
        expiration,
        claim_pk,
        gateway_info.module_public_key,
        ephemeral_keypair.public_key(),
    );

    let invoice = gateway_request::<CreateBolt11InvoiceResponse>(
        GatewayMethod::CreateBolt11Invoice(CreateBolt11InvoiceRequest {
            federation_id,
            contract: contract.clone(),
            amount: Amount::from_msats(amount),
            description: Bolt11InvoiceDescription::Direct("LNURL Payment".to_string()),
            expiry_secs,
        }),
        &connection,
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

    Ok((gateway, invoice))
}

async fn select_gateway(
    endpoint: &Endpoint,
    gateways: &[iroh::PublicKey],
    federation_id: FederationId,
) -> anyhow::Result<(Connection, GatewayInfo, iroh::PublicKey)> {
    for gateway in gateways {
        let Ok(connection) = dial(endpoint, *gateway).await else {
            continue;
        };
        if let Ok(resp) = gateway_request::<GatewayInfoResponse>(
            GatewayMethod::GatewayInfo(GatewayInfoRequest { federation_id }),
            &connection,
        )
        .await
        {
            return Ok((connection, resp.gateway_info, *gateway));
        }
    }

    bail!("All gateways are offline or do not support this federation")
}

async fn dial(endpoint: &Endpoint, node_id: iroh::PublicKey) -> anyhow::Result<Connection> {
    endpoint
        .connect(node_id, PICOMINT_ALPN)
        .await
        .map_err(|e| anyhow!("connect: {e}"))
}

/// One-shot iroh bi-stream request mirroring
/// `picomint_client::ln::gateway_api::request`. Duplicated here because
/// recurringd doesn't depend on picomint-client.
async fn gateway_request<Resp: Decodable>(
    method: GatewayMethod,
    connection: &Connection,
) -> anyhow::Result<Resp> {
    let request_bytes = method.consensus_encode_to_vec();

    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|e| anyhow!("open_bi: {e}"))?;

    send.write_all(&request_bytes)
        .await
        .map_err(|e| anyhow!("write: {e}"))?;
    send.finish().map_err(|e| anyhow!("finish: {e}"))?;

    let response_bytes = recv
        .read_to_end(GATEWAY_MAX_MESSAGE_BYTES)
        .await
        .map_err(|e| anyhow!("read: {e}"))?;

    let result = <Result<Vec<u8>, ApiError>>::consensus_decode_exact(&response_bytes)
        .map_err(|e| anyhow!("decode envelope: {e}"))?;

    let payload = result.map_err(|e| anyhow!("gateway error: {e}"))?;

    Resp::consensus_decode_exact(&payload).map_err(|e| anyhow!("decode payload: {e}"))
}
