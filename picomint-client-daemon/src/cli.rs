//! The admin socket: one route per client call.

use std::future::Future;

use axum::Router;
use axum::extract::{Json, State};
use axum::routing::post;
use picomint_cli_server::{CliError, serve};
use picomint_client_cli_core::{
    ClientAddRequest, ClientBalanceRequest, ClientBalanceResponse, ClientConfigRequest,
    ClientConfigResponse, ClientEcashCountRequest, ClientEcashCountResponse,
    ClientEcashReceiveRequest, ClientEcashReceiveResponse, ClientEcashSendMaxRequest,
    ClientEcashSendMaxResponse, ClientEcashSendRequest, ClientEcashSendResponse,
    ClientLightningGatewayListRequest, ClientLightningGatewayListResponse,
    ClientLightningGatewayRefreshRequest, ClientLightningLnurlRequest,
    ClientLightningLnurlResponse, ClientLightningReceiveRequest, ClientLightningReceiveResponse,
    ClientLightningSendMaxRequest, ClientLightningSendMaxResponse, ClientLightningSendRequest,
    ClientLightningSendResponse, ClientListResponse, ClientOnchainReceiveRequest,
    ClientOnchainReceiveResponse, ClientOnchainSendFeeRequest, ClientOnchainSendFeeResponse,
    ClientOnchainSendMaxRequest, ClientOnchainSendMaxResponse, ClientOnchainSendRequest,
    ClientOnchainSendResponse, ClientRemoveRequest, MintInfo, MnemonicResponse, QueryRequest,
    QueryResponse, ROUTE_ADD, ROUTE_BALANCE, ROUTE_CONFIG, ROUTE_ECASH_COUNT, ROUTE_ECASH_RECEIVE,
    ROUTE_ECASH_SEND, ROUTE_ECASH_SEND_MAX, ROUTE_LIGHTNING_GATEWAY_LIST,
    ROUTE_LIGHTNING_GATEWAY_REFRESH, ROUTE_LIGHTNING_LNURL, ROUTE_LIGHTNING_RECEIVE,
    ROUTE_LIGHTNING_SEND, ROUTE_LIGHTNING_SEND_MAX, ROUTE_LIST, ROUTE_MNEMONIC,
    ROUTE_ONCHAIN_RECEIVE, ROUTE_ONCHAIN_SEND, ROUTE_ONCHAIN_SEND_FEE, ROUTE_ONCHAIN_SEND_MAX,
    ROUTE_QUERY, ROUTE_REMOVE,
};
use picomint_core::Amount;
use tracing::instrument;

use crate::AppState;

pub fn run(state: AppState) -> anyhow::Result<impl Future<Output = ()>> {
    let data_dir = state.data_dir.clone();

    let router = Router::new()
        .route(ROUTE_MNEMONIC, post(mnemonic))
        .route(ROUTE_QUERY, post(query))
        .route(ROUTE_ADD, post(add))
        .route(ROUTE_REMOVE, post(remove))
        .route(ROUTE_LIST, post(list))
        .route(ROUTE_CONFIG, post(config))
        .route(ROUTE_BALANCE, post(balance))
        .route(ROUTE_ECASH_COUNT, post(ecash_count))
        .route(ROUTE_ECASH_SEND, post(ecash_send))
        .route(ROUTE_ECASH_SEND_MAX, post(ecash_send_max))
        .route(ROUTE_ECASH_RECEIVE, post(ecash_receive))
        .route(ROUTE_ONCHAIN_SEND_FEE, post(onchain_send_fee))
        .route(ROUTE_ONCHAIN_SEND, post(onchain_send))
        .route(ROUTE_ONCHAIN_SEND_MAX, post(onchain_send_max))
        .route(ROUTE_ONCHAIN_RECEIVE, post(onchain_receive))
        .route(ROUTE_LIGHTNING_GATEWAY_LIST, post(lightning_gateway_list))
        .route(ROUTE_LIGHTNING_SEND, post(lightning_send))
        .route(ROUTE_LIGHTNING_SEND_MAX, post(lightning_send_max))
        .route(ROUTE_LIGHTNING_RECEIVE, post(lightning_receive))
        .route(ROUTE_LIGHTNING_LNURL, post(lightning_lnurl))
        .route(
            ROUTE_LIGHTNING_GATEWAY_REFRESH,
            post(lightning_gateway_refresh),
        )
        .with_state(state);

    serve(&data_dir, router)
}

#[instrument(skip_all, err)]
async fn mnemonic(State(state): State<AppState>) -> Result<Json<MnemonicResponse>, CliError> {
    let mnemonic = state
        .mnemonic
        .words()
        .map(std::string::ToString::to_string)
        .collect();

    Ok(Json(MnemonicResponse { mnemonic }))
}

#[instrument(skip_all, err)]
async fn query(
    State(state): State<AppState>,
    Json(request): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, CliError> {
    let rows = tokio::task::spawn_blocking(move || {
        picomint_analytics::query(&state.data_dir, &request.query)
    })
    .await
    .map_err(CliError::internal)?
    .map_err(CliError::bad_request)?;

    Ok(Json(rows))
}

#[instrument(skip_all, err)]
async fn add(
    State(state): State<AppState>,
    Json(payload): Json<ClientAddRequest>,
) -> Result<Json<()>, CliError> {
    state
        .client
        .add_mint(&payload.invite, Some(state.network))
        .await?;

    Ok(Json(()))
}

#[instrument(skip_all, err)]
async fn remove(
    State(state): State<AppState>,
    Json(payload): Json<ClientRemoveRequest>,
) -> Result<Json<()>, CliError> {
    state.client.begin_remove_mint(payload.mint).await?.commit();

    Ok(Json(()))
}

#[instrument(skip_all, err)]
async fn list(State(state): State<AppState>) -> Result<Json<ClientListResponse>, CliError> {
    let mints = state
        .client
        .mint_configs()
        .into_iter()
        .map(|entry| MintInfo {
            mint: entry.0,
            mint_name: entry.1.name,
        })
        .collect();

    Ok(Json(ClientListResponse { mints }))
}

#[instrument(skip_all, err)]
async fn config(
    State(state): State<AppState>,
    Json(payload): Json<ClientConfigRequest>,
) -> Result<Json<ClientConfigResponse>, CliError> {
    let config = state
        .client
        .config(payload.mint)
        .ok_or_else(|| CliError::bad_request("Mint not added"))?;

    Ok(Json(ClientConfigResponse {
        config: serde_json::to_value(config).expect("ConsensusConfig is serializable"),
    }))
}

#[instrument(skip_all, err)]
async fn balance(
    State(state): State<AppState>,
    Json(payload): Json<ClientBalanceRequest>,
) -> Result<Json<ClientBalanceResponse>, CliError> {
    let balance_msat = state.client.ecash_balance(payload.mint, payload.account);

    Ok(Json(ClientBalanceResponse { balance_msat }))
}

#[instrument(skip_all, err)]
async fn ecash_count(
    State(state): State<AppState>,
    Json(payload): Json<ClientEcashCountRequest>,
) -> Result<Json<ClientEcashCountResponse>, CliError> {
    let counts = state.client.ecash_count(payload.mint, payload.account);

    Ok(Json(ClientEcashCountResponse { counts }))
}

#[instrument(skip_all, err)]
async fn ecash_send(
    State(state): State<AppState>,
    Json(payload): Json<ClientEcashSendRequest>,
) -> Result<Json<ClientEcashSendResponse>, CliError> {
    let ecash = state
        .client
        .ecash_send(
            payload.mint,
            payload.account,
            Amount::from_sat(payload.amount.to_sat()),
        )
        .await
        .map_err(CliError::internal)?;

    Ok(Json(ClientEcashSendResponse { ecash }))
}

#[instrument(skip_all, err)]
async fn ecash_send_max(
    State(state): State<AppState>,
    Json(payload): Json<ClientEcashSendMaxRequest>,
) -> Result<Json<ClientEcashSendMaxResponse>, CliError> {
    let ecash = state
        .client
        .ecash_send_max(payload.mint, payload.account)
        .map_err(CliError::internal)?;

    Ok(Json(ClientEcashSendMaxResponse { ecash }))
}

#[instrument(skip_all, err)]
async fn ecash_receive(
    State(state): State<AppState>,
    Json(payload): Json<ClientEcashReceiveRequest>,
) -> Result<Json<ClientEcashReceiveResponse>, CliError> {
    let operation = state
        .client
        .ecash_receive(payload.mint, payload.account, &payload.ecash)
        .map_err(CliError::internal)?;

    Ok(Json(ClientEcashReceiveResponse { operation }))
}

#[instrument(skip_all, err)]
async fn onchain_send_fee(
    State(state): State<AppState>,
    Json(payload): Json<ClientOnchainSendFeeRequest>,
) -> Result<Json<ClientOnchainSendFeeResponse>, CliError> {
    let fee = state
        .client
        .onchain_send_fee(payload.mint)
        .await
        .map_err(CliError::internal)?;

    Ok(Json(ClientOnchainSendFeeResponse { fee }))
}

#[instrument(skip_all, err)]
async fn onchain_send(
    State(state): State<AppState>,
    Json(payload): Json<ClientOnchainSendRequest>,
) -> Result<Json<ClientOnchainSendResponse>, CliError> {
    let operation = state
        .client
        .onchain_send(
            payload.mint,
            payload.account,
            payload.address,
            payload.amount,
            payload.fee,
        )
        .await
        .map_err(CliError::internal)?;

    Ok(Json(ClientOnchainSendResponse { operation }))
}

#[instrument(skip_all, err)]
async fn onchain_send_max(
    State(state): State<AppState>,
    Json(payload): Json<ClientOnchainSendMaxRequest>,
) -> Result<Json<ClientOnchainSendMaxResponse>, CliError> {
    let operation = state
        .client
        .onchain_send_max(payload.mint, payload.account, payload.address)
        .await
        .map_err(CliError::internal)?;

    Ok(Json(ClientOnchainSendMaxResponse { operation }))
}

#[instrument(skip_all, err)]
async fn onchain_receive(
    State(state): State<AppState>,
    Json(payload): Json<ClientOnchainReceiveRequest>,
) -> Result<Json<ClientOnchainReceiveResponse>, CliError> {
    let address = state
        .client
        .onchain_receive(payload.mint, payload.account)
        .map_err(CliError::internal)?;

    Ok(Json(ClientOnchainReceiveResponse {
        address: address.as_unchecked().clone(),
    }))
}

#[instrument(skip_all, err)]
async fn lightning_gateway_list(
    State(state): State<AppState>,
    Json(payload): Json<ClientLightningGatewayListRequest>,
) -> Result<Json<ClientLightningGatewayListResponse>, CliError> {
    let gateways = state
        .client
        .lightning_gateways(payload.mint)
        .map_err(CliError::internal)?;

    Ok(Json(ClientLightningGatewayListResponse { gateways }))
}

#[instrument(skip_all, err)]
async fn lightning_send(
    State(state): State<AppState>,
    Json(payload): Json<ClientLightningSendRequest>,
) -> Result<Json<ClientLightningSendResponse>, CliError> {
    let operation = state
        .client
        .lightning_send(
            payload.mint,
            payload.account,
            payload.gateway,
            payload.invoice,
        )
        .await
        .map_err(CliError::internal)?;

    Ok(Json(ClientLightningSendResponse { operation }))
}

#[instrument(skip_all, err)]
async fn lightning_send_max(
    State(state): State<AppState>,
    Json(payload): Json<ClientLightningSendMaxRequest>,
) -> Result<Json<ClientLightningSendMaxResponse>, CliError> {
    let operation = state
        .client
        .lightning_send_max(
            payload.mint,
            payload.account,
            payload.gateway,
            &payload.lnurl,
        )
        .await
        .map_err(CliError::internal)?;

    Ok(Json(ClientLightningSendMaxResponse { operation }))
}

#[instrument(skip_all, err)]
async fn lightning_receive(
    State(state): State<AppState>,
    Json(payload): Json<ClientLightningReceiveRequest>,
) -> Result<Json<ClientLightningReceiveResponse>, CliError> {
    let invoice = state
        .client
        .lightning_receive(
            payload.mint,
            payload.account,
            payload.gateway,
            Amount::from_sat(payload.amount.to_sat()),
        )
        .await
        .map_err(CliError::internal)?;

    Ok(Json(ClientLightningReceiveResponse { invoice }))
}

#[instrument(skip_all, err)]
async fn lightning_lnurl(
    State(state): State<AppState>,
    Json(payload): Json<ClientLightningLnurlRequest>,
) -> Result<Json<ClientLightningLnurlResponse>, CliError> {
    let lnurl = state
        .client
        .lightning_generate_lnurl(payload.mint, payload.account, payload.lnurl_daemon)
        .map_err(CliError::internal)?;

    Ok(Json(ClientLightningLnurlResponse { lnurl }))
}

#[instrument(skip_all, err)]
async fn lightning_gateway_refresh(
    State(state): State<AppState>,
    Json(payload): Json<ClientLightningGatewayRefreshRequest>,
) -> Result<Json<()>, CliError> {
    state
        .client
        .lightning_refresh_gateways(payload.mint)
        .await
        .map_err(CliError::internal)?;

    Ok(Json(()))
}
