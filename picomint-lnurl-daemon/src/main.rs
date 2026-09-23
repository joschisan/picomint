use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use anyhow::{Context, bail, ensure};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use bitcoin::hashes::sha256;
use clap::Parser;
use futures::future::select_ok;
use iroh::Endpoint;
use iroh::endpoint::presets::N0;
use iroh_mdns_address_lookup::MdnsAddressLookup;
use lightning_invoice::Bolt11Invoice;
use picomint_core::config::MintId;
use picomint_core::lightning::MINIMUM_INCOMING_CONTRACT_AMOUNT;
use picomint_core::lightning::contracts::IncomingContract;
use picomint_core::lightning::gateway::{GatewayInfo, GatewayPk, PaymentFee};
use picomint_core::lightning::lnurl::{LnurlRequest, MAX_NODES_PER_LNURL};
use picomint_core::lightning::methods::{
    AwaitIncomingPaymentRequest, AwaitIncomingPaymentResponse, GatewayMethod, GatewaysRequest,
    GatewaysResponse, IncomingPaymentRequest, IncomingPaymentResponse, InfoRequest, InfoResponse,
    LightningMethod, ReceiveRequest, ReceiveResponse,
};
use picomint_core::methods::{CoreMethod, Method, MintInfoRequest, MintInfoResponse};
use picomint_core::{Amount, NodeId};
use picomint_encoding::{Decodable, Encodable};
use picomint_lnurl::{
    InvoiceResponse, LnurlResponse, PayResponse, VerifyResponse, pay_request_tag,
};
use picomint_rpc::api::MintApi;
use serde::Deserialize;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tower_http::cors;
use tower_http::cors::CorsLayer;
use tracing::info;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const MAX_SENDABLE_MSAT: u64 = 100_000_000_000;
const MIN_SENDABLE_MSAT: u64 = 100_000;

/// The daemon's state: its endpoint and one kept-alive [`MintApi`] per
/// mint it has served an invoice for, keyed by mint id. The pool's
/// connections reconnect on their own, so a mint seen once is warm for
/// every request after; a verify finds its mint here by id alone, and so
/// only on the process that issued the invoice.
#[derive(Clone)]
struct AppState {
    endpoint: Endpoint,
    mints: Arc<RwLock<BTreeMap<MintId, MintApi>>>,
}

impl AppState {
    /// The pool for `mint`, built from `nodes` on first sight.
    fn mint_api(&self, mint: MintId, nodes: BTreeMap<NodeId, iroh::PublicKey>) -> MintApi {
        self.mints
            .write()
            .expect("mints RwLock poisoned")
            .entry(mint)
            .or_insert_with(|| MintApi::new(self.endpoint.clone(), nodes))
            .clone()
    }

    fn cached_mint_api(&self, mint: MintId) -> Option<MintApi> {
        self.mints
            .read()
            .expect("mints RwLock poisoned")
            .get(&mint)
            .cloned()
    }
}

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
        .transport_config(picomint_rpc::transport_config())
        .address_lookup(MdnsAddressLookup::builder())
        .bind()
        .await?;

    let cors = CorsLayer::new()
        .allow_origin(cors::Any)
        .allow_methods(cors::Any)
        .allow_headers(cors::Any);

    let state = AppState {
        endpoint,
        mints: Arc::new(RwLock::new(BTreeMap::new())),
    };

    let app = Router::new()
        .route("/", get(health_check))
        .route("/pay/{payload}", get(pay))
        .route("/invoice/{payload}", get(invoice))
        .route("/verify/{mint}/{payment_hash}", get(verify))
        .layer(cors)
        .with_state(state);

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
    State(state): State<AppState>,
    Path(payload): Path<String>,
    Query(params): Query<GetInvoiceParams>,
) -> Json<LnurlResponse<InvoiceResponse>> {
    let Ok(request) = picomint_base32::decode::<LnurlRequest>(&payload) else {
        return Json(LnurlResponse::error("Failed to decode payload"));
    };

    if request.nodes.len() > MAX_NODES_PER_LNURL {
        return Json(LnurlResponse::error(format!(
            "Too many nodes in request (max {MAX_NODES_PER_LNURL})"
        )));
    }

    if params.amount < MIN_SENDABLE_MSAT || params.amount > MAX_SENDABLE_MSAT {
        return Json(LnurlResponse::error(format!(
            "Amount must be between {} and {}",
            MIN_SENDABLE_MSAT, MAX_SENDABLE_MSAT
        )));
    }

    let (mint, invoice) = match resolve_and_fetch_invoice(&state, &request, params.amount).await {
        Ok(result) => result,
        Err(e) => {
            return Json(LnurlResponse::error(e.to_string()));
        }
    };

    info!(%params.amount, "Created invoice");

    // The verify URL routes through this daemon, which asks the mint —
    // found by id in the pool this request just warmed — whether a
    // contract with the invoice's payment hash was funded.
    let verify = format!(
        "{}verify/{mint}/{}",
        base_url(&headers),
        invoice.payment_hash()
    );

    Json(LnurlResponse::Ok(InvoiceResponse {
        pr: invoice.clone(),
        verify: Some(verify),
    }))
}

/// Resolve the mint from the payload's nodes, then buy an invoice
/// from one of its currently announced gateways. Nothing perishable comes out
/// of the lnurl itself, which is what keeps an outstanding one valid across
/// gateway churn.
async fn resolve_and_fetch_invoice(
    state: &AppState,
    request: &LnurlRequest,
    amount: u64,
) -> anyhow::Result<(MintId, Bolt11Invoice)> {
    let (info, api) = resolve_mint(state, request).await?;

    let gateways = fetch_gateways(&api).await?;

    let (gateway_info, gateway_pk) = select_gateway(&state.endpoint, gateways, info.mint).await?;

    ensure!(
        gateway_info
            .receive_fee
            .is_within(&PaymentFee::RECEIVE_FEE_LIMIT),
        "Payment fee exceeds limit"
    );

    let fee = gateway_info.receive_fee.fee(amount);

    ensure!(
        amount
            .checked_sub(fee.0)
            .is_some_and(|net| Amount(net) >= MINIMUM_INCOMING_CONTRACT_AMOUNT),
        "Amount too small"
    );

    let contract = IncomingContract::author(&request.recipient, Amount(amount), fee);

    let payment_hash = contract.payment_hash();

    let receive = ReceiveRequest {
        mint: info.mint,
        contract,
    };

    let invoice = gateway_request::<ReceiveResponse>(
        &state.endpoint,
        gateway_pk,
        GatewayMethod::Receive(receive),
    )
    .await?
    .invoice;

    ensure!(
        invoice.payment_hash() == &payment_hash,
        "Invalid invoice payment hash"
    );

    ensure!(
        invoice.amount_milli_satoshis() == Some(amount),
        "Invalid invoice amount"
    );

    Ok((info.mint, invoice))
}

/// The mint an lnurl payload names, as its info and the pooled API onto
/// its full node set. The info is fetched every time, since it is what
/// the payload commits to; the pool is built once per mint.
async fn resolve_mint(
    state: &AppState,
    request: &LnurlRequest,
) -> anyhow::Result<(MintInfoResponse, MintApi)> {
    let info = fetch_mint_info(&state.endpoint, &request.nodes, request.info).await?;

    let nodes = info
        .nodes
        .iter()
        .map(|(node, endpoint)| (*node, endpoint.iroh_pk))
        .collect();

    let api = state.mint_api(info.mint, nodes);

    Ok((info, api))
}

/// Take the first node response that hashes to the payload's commitment.
/// That commitment is what makes a single node enough: one can stall or
/// refuse, but a forged node set will not hash. The payload carries `f + 1` of
/// them, so one is honest and reachable whenever the mint itself is.
///
/// One request per node and no reuse, so this dials directly rather than
/// standing up a [`MintApi`] — and the nodes are a subset, which a
/// mint-shaped node set has no room for.
async fn fetch_mint_info(
    endpoint: &Endpoint,
    nodes: &[iroh::PublicKey],
    info: sha256::Hash,
) -> anyhow::Result<MintInfoResponse> {
    ensure!(!nodes.is_empty(), "Lnurl names no nodes");

    let attempts = nodes.iter().copied().map(|node| {
        Box::pin(async move {
            let response: MintInfoResponse = picomint_rpc::request(
                endpoint,
                node,
                Method::Core(CoreMethod::MintInfo(MintInfoRequest)),
            )
            .await?;

            ensure!(
                response.consensus_hash_sha256() == info,
                "Response does not hash to the lnurl's commitment"
            );

            anyhow::Ok(response)
        })
    });

    let response = select_ok(attempts)
        .await
        .context("No node served an info matching the lnurl's commitment")?
        .0;

    Ok(response)
}

/// Threshold-read the mint's announced gateway set — `2f + 1` nodes
/// returning byte-identical lists. Not committed to by the lnurl: the
/// node set it is read from is, and `2f + 1` nodes agreeing on a value is
/// the same assumption the rest of the mint already rests on.
async fn fetch_gateways(api: &MintApi) -> anyhow::Result<Vec<GatewayPk>> {
    let response: GatewaysResponse = api
        .request_current_consensus(Method::Lightning(LightningMethod::Gateways(
            GatewaysRequest,
        )))
        .await?;

    Ok(response.gateways)
}

async fn select_gateway(
    endpoint: &Endpoint,
    gateways: Vec<GatewayPk>,
    mint: MintId,
) -> anyhow::Result<(GatewayInfo, GatewayPk)> {
    let mut probes = JoinSet::new();

    for gateway_pk in gateways {
        let endpoint = endpoint.clone();
        probes.spawn(async move {
            let response = gateway_request::<InfoResponse>(
                &endpoint,
                gateway_pk,
                GatewayMethod::Info(InfoRequest { mint }),
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

    bail!("All gateways are offline or do not support this mint")
}

/// LUD-21 verify: an external LNURL wallet hits us at
/// `/verify/{mint}/{payment_hash}` (URL embedded in the invoice
/// response), and we ask the mint, under threshold consensus, whether a
/// contract with that payment hash was funded. The hash names the one
/// contract this daemon authored for the invoice, so that is the mint's
/// own attestation that the recipient was paid what the invoice said,
/// with the preimage as proof. The optional `?wait` query param
/// long-polls the mint until it was; without it the mint answers with
/// what it holds now.
///
/// The mint is found by id in the pool the invoice request warmed. A
/// mint id names no nodes, so a process that never issued the invoice —
/// after a restart, or another replica — has no way to the mint and
/// answers as a transport failure would.
///
/// LUD-21 has no transient-vs-terminal error distinction — a wallet
/// that sees `{"status":"ERROR"}` (or once `settled:true`, later
/// `settled:false`) will give up. So on transport failure we return
/// HTTP 502 with an empty body: the wallet's JSON parse fails the same
/// way as a network error, and any sane polling client retries.
async fn verify(
    State(state): State<AppState>,
    Path((mint, payment_hash)): Path<(MintId, sha256::Hash)>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<LnurlResponse<VerifyResponse>>, StatusCode> {
    let api = state.cached_mint_api(mint).ok_or(StatusCode::BAD_GATEWAY)?;

    // A waited read never gives up: the mint API keeps asking every node
    // across reconnects until a threshold agrees, which is what waiting
    // means. An unwaited read fails once enough nodes do, and a polling
    // wallet asks again.
    let preimage = if query.contains_key("wait") {
        Some(
            api.request_current_consensus_retry::<AwaitIncomingPaymentResponse>(Method::Lightning(
                LightningMethod::AwaitIncomingPayment(AwaitIncomingPaymentRequest { payment_hash }),
            ))
            .await
            .preimage,
        )
    } else {
        api.request_current_consensus::<IncomingPaymentResponse>(Method::Lightning(
            LightningMethod::IncomingPayment(IncomingPaymentRequest { payment_hash }),
        ))
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?
        .preimage
    };

    Ok(Json(LnurlResponse::Ok(VerifyResponse {
        settled: preimage.is_some(),
        preimage,
    })))
}

async fn gateway_request<R: Decodable>(
    endpoint: &Endpoint,
    gateway_pk: GatewayPk,
    method: GatewayMethod,
) -> anyhow::Result<R> {
    picomint_rpc::request(endpoint, gateway_pk.0, method).await
}
