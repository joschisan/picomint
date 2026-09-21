use std::future::Future;

use axum::Router;
use axum::extract::{Json, State};
use axum::routing::post;
use picomint_broker_cli_core::{
    ClientAddRequest, ClientAddResponse, ClientBalanceRequest, ClientBalanceResponse,
    ClientConfigRequest, ClientConfigResponse, ClientEcashCountRequest, ClientEcashCountResponse,
    ClientEcashReceiveRequest, ClientEcashReceiveResponse, ClientEcashSendMaxRequest,
    ClientEcashSendMaxResponse, ClientEcashSendRequest, ClientEcashSendResponse,
    ClientListResponse, ClientOnchainReceiveFeeRequest, ClientOnchainReceiveFeeResponse,
    ClientOnchainReceiveRequest, ClientOnchainReceiveResponse, ClientOnchainSendFeeRequest,
    ClientOnchainSendFeeResponse, ClientOnchainSendMaxRequest, ClientOnchainSendMaxResponse,
    ClientOnchainSendRequest, ClientOnchainSendResponse, ClientRemoveRequest, InfoResponse,
    MnemonicResponse, QueryRequest, QueryResponse, ROUTE_CLIENT_ADD, ROUTE_CLIENT_BALANCE,
    ROUTE_CLIENT_CONFIG, ROUTE_CLIENT_ECASH_COUNT, ROUTE_CLIENT_ECASH_RECEIVE,
    ROUTE_CLIENT_ECASH_SEND, ROUTE_CLIENT_ECASH_SEND_MAX, ROUTE_CLIENT_LIST,
    ROUTE_CLIENT_ONCHAIN_RECEIVE, ROUTE_CLIENT_ONCHAIN_RECEIVE_FEE, ROUTE_CLIENT_ONCHAIN_SEND,
    ROUTE_CLIENT_ONCHAIN_SEND_FEE, ROUTE_CLIENT_ONCHAIN_SEND_MAX, ROUTE_CLIENT_REMOVE, ROUTE_INFO,
    ROUTE_MNEMONIC, ROUTE_QUERY, ROUTE_REBALANCE, RebalanceResponse,
};
use picomint_cli_server::{CliError, serve};
use picomint_client::NotAddedError;
use picomint_core::swap::broker::BrokerPk;
use tracing::{info, instrument};

use crate::AppState;

pub fn run(state: AppState) -> anyhow::Result<impl Future<Output = ()>> {
    serve(&state.data_dir.clone(), router().with_state(state))
}

fn router() -> Router<AppState> {
    Router::new()
        .route(ROUTE_INFO, post(info))
        .route(ROUTE_MNEMONIC, post(mnemonic))
        .route(ROUTE_QUERY, post(query))
        .route(ROUTE_REBALANCE, post(rebalance))
        .route(ROUTE_CLIENT_ADD, post(client_add))
        .route(ROUTE_CLIENT_REMOVE, post(client_remove))
        .route(ROUTE_CLIENT_LIST, post(client_list))
        .route(ROUTE_CLIENT_CONFIG, post(client_config))
        .route(ROUTE_CLIENT_BALANCE, post(client_balance))
        .route(ROUTE_CLIENT_ECASH_COUNT, post(client_ecash_count))
        .route(ROUTE_CLIENT_ECASH_SEND, post(client_ecash_send))
        .route(ROUTE_CLIENT_ECASH_SEND_MAX, post(client_ecash_send_max))
        .route(ROUTE_CLIENT_ECASH_RECEIVE, post(client_ecash_receive))
        .route(ROUTE_CLIENT_ONCHAIN_SEND_FEE, post(client_onchain_send_fee))
        .route(
            ROUTE_CLIENT_ONCHAIN_RECEIVE_FEE,
            post(client_onchain_receive_fee),
        )
        .route(ROUTE_CLIENT_ONCHAIN_SEND, post(client_onchain_send))
        .route(ROUTE_CLIENT_ONCHAIN_SEND_MAX, post(client_onchain_send_max))
        .route(ROUTE_CLIENT_ONCHAIN_RECEIVE, post(client_onchain_receive))
}

#[instrument(skip_all, err)]
async fn info(State(state): State<AppState>) -> Result<Json<InfoResponse>, CliError> {
    Ok(Json(InfoResponse {
        broker_pk: BrokerPk(state.endpoint.id()),
        network: state.network.to_string(),
        fee: state.fee,
    }))
}

#[instrument(skip_all, err)]
async fn mnemonic(State(state): State<AppState>) -> Result<Json<MnemonicResponse>, CliError> {
    let words = state
        .mnemonic
        .words()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>();

    Ok(Json(MnemonicResponse { mnemonic: words }))
}

async fn query(
    State(state): State<AppState>,
    Json(request): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, CliError> {
    let rows = tokio::task::spawn_blocking(move || {
        picomint_analytics::query(&state.data_dir, &request.query)
    })
    .await
    .expect("the query task is not cancelled")
    .map_err(CliError::rejected)?;

    Ok(Json(QueryResponse(rows)))
}

#[instrument(skip_all, err)]
async fn rebalance(
    State(state): State<AppState>,
) -> Result<Json<Option<RebalanceResponse>>, CliError> {
    state
        .rebalance()
        .await
        .map(Json)
        .map_err(CliError::rejected)
}

#[instrument(skip_all, err)]
async fn client_add(
    State(state): State<AppState>,
    Json(payload): Json<ClientAddRequest>,
) -> Result<Json<ClientAddResponse>, CliError> {
    let mint = state
        .client
        .add_mint(&payload.invite, Some(state.network))
        .await
        .map_err(CliError::rejected)?;

    Ok(Json(ClientAddResponse { mint }))
}

/// Remove a mint: shut its client runtime down, then delete its client
/// rows. Destructive — in-flight swaps are dropped with their state
/// machines, so the operator checks for in-flight swaps via the query
/// route before removing; failing to check might result in loss of funds.
#[instrument(skip_all, err)]
async fn client_remove(
    State(state): State<AppState>,
    Json(payload): Json<ClientRemoveRequest>,
) -> Result<Json<()>, CliError> {
    state
        .client
        .begin_remove_mint(payload.mint)
        .await
        .map_err(CliError::rejected)?
        .commit();

    info!(mint = %payload.mint, "Removed mint");

    Ok(Json(()))
}

#[instrument(skip_all, err)]
async fn client_list(State(state): State<AppState>) -> Result<Json<ClientListResponse>, CliError> {
    Ok(Json(ClientListResponse {
        mints: state.mint_list(),
    }))
}

#[instrument(skip_all, err)]
async fn client_config(
    State(state): State<AppState>,
    Json(payload): Json<ClientConfigRequest>,
) -> Result<Json<ClientConfigResponse>, CliError> {
    let config = state
        .client
        .config(payload.mint)
        .ok_or_else(|| CliError::rejected(NotAddedError::NotAdded))?;

    Ok(Json(ClientConfigResponse {
        config: serde_json::to_value(config).expect("NodeConfigConsensus is serializable"),
    }))
}

#[instrument(skip_all, err)]
async fn client_balance(
    State(state): State<AppState>,
    Json(payload): Json<ClientBalanceRequest>,
) -> Result<Json<ClientBalanceResponse>, CliError> {
    let balance_msat = state.client.ecash_balance(payload.mint, payload.account);

    Ok(Json(ClientBalanceResponse { balance_msat }))
}

#[instrument(skip_all, err)]
async fn client_ecash_count(
    State(state): State<AppState>,
    Json(payload): Json<ClientEcashCountRequest>,
) -> Result<Json<ClientEcashCountResponse>, CliError> {
    let counts = state.client.ecash_count(payload.mint, payload.account);

    Ok(Json(ClientEcashCountResponse { counts }))
}

#[instrument(skip_all, err)]
async fn client_ecash_send(
    State(state): State<AppState>,
    Json(payload): Json<ClientEcashSendRequest>,
) -> Result<Json<ClientEcashSendResponse>, CliError> {
    let ecash = state
        .client
        .ecash_send(
            payload.mint,
            payload.account,
            picomint_core::Amount::from_sat(payload.amount.to_sat()),
        )
        .await
        .map_err(CliError::rejected)?;

    Ok(Json(ClientEcashSendResponse { ecash }))
}

#[instrument(skip_all, err)]
async fn client_ecash_send_max(
    State(state): State<AppState>,
    Json(payload): Json<ClientEcashSendMaxRequest>,
) -> Result<Json<ClientEcashSendMaxResponse>, CliError> {
    let ecash = state
        .client
        .ecash_send_max(payload.mint, payload.account)
        .map_err(CliError::rejected)?;

    Ok(Json(ClientEcashSendMaxResponse { ecash }))
}

#[instrument(skip_all, err)]
async fn client_ecash_receive(
    State(state): State<AppState>,
    Json(payload): Json<ClientEcashReceiveRequest>,
) -> Result<Json<ClientEcashReceiveResponse>, CliError> {
    let operation = state
        .client
        .ecash_receive(payload.mint, payload.account, &payload.ecash)
        .map_err(CliError::rejected)?;

    Ok(Json(ClientEcashReceiveResponse { operation }))
}

#[instrument(skip_all, err)]
async fn client_onchain_send_fee(
    State(state): State<AppState>,
    Json(payload): Json<ClientOnchainSendFeeRequest>,
) -> Result<Json<ClientOnchainSendFeeResponse>, CliError> {
    let fee = state
        .client
        .onchain_send_fee(payload.mint)
        .await
        .map_err(CliError::rejected)?;

    Ok(Json(ClientOnchainSendFeeResponse { fee }))
}

#[instrument(skip_all, err)]
async fn client_onchain_receive_fee(
    State(state): State<AppState>,
    Json(payload): Json<ClientOnchainReceiveFeeRequest>,
) -> Result<Json<ClientOnchainReceiveFeeResponse>, CliError> {
    let fee = state
        .client
        .onchain_receive_fee(payload.mint)
        .await
        .map_err(CliError::rejected)?;

    Ok(Json(ClientOnchainReceiveFeeResponse { fee }))
}

#[instrument(skip_all, err)]
async fn client_onchain_send(
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
        .map_err(CliError::rejected)?;

    Ok(Json(ClientOnchainSendResponse { operation }))
}

#[instrument(skip_all, err)]
async fn client_onchain_send_max(
    State(state): State<AppState>,
    Json(payload): Json<ClientOnchainSendMaxRequest>,
) -> Result<Json<ClientOnchainSendMaxResponse>, CliError> {
    let operation = state
        .client
        .onchain_send_max(payload.mint, payload.account, payload.address)
        .await
        .map_err(CliError::rejected)?;

    Ok(Json(ClientOnchainSendMaxResponse { operation }))
}

#[instrument(skip_all, err)]
async fn client_onchain_receive(
    State(state): State<AppState>,
    Json(payload): Json<ClientOnchainReceiveRequest>,
) -> Result<Json<ClientOnchainReceiveResponse>, CliError> {
    let address = state
        .client
        .onchain_receive(payload.mint, payload.account)
        .map_err(CliError::rejected)?;

    Ok(Json(ClientOnchainReceiveResponse {
        address: address.as_unchecked().clone(),
    }))
}
