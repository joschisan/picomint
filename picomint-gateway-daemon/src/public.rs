//! Public client-facing API served over iroh. One bi-stream per request,
//! consensus-encoded, mirrors the federation server's `run_iroh_api` shape
//! with a flat `GatewayMethod` dispatch since the gateway has no modules.

use std::sync::Arc;

use iroh::Endpoint;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use picomint_core::ln::gateway_api::{
    CreateBolt11InvoiceRequest, CreateBolt11InvoiceResponse, GATEWAY_MAX_MESSAGE_BYTES,
    GatewayInfoRequest, GatewayInfoResponse, GatewayMethod, SendPaymentRequest,
    SendPaymentResponse, VerifyBolt11PreimageRequest, VerifyBolt11PreimageResponse,
};
use picomint_core::module::ApiError;
use picomint_core::task::TaskHandle;
use picomint_encoding::{Decodable, Encodable};
use picomint_lnurl::VerifyResponse;
use picomint_logging::LOG_GATEWAY;
use tokio::sync::Semaphore;
use tracing::{debug, warn};

use crate::AppState;

/// Same limits as the federation server's iroh API. Gateways are simpler
/// so these are safe defaults.
const MAX_CONNECTIONS: usize = 1000;
const MAX_REQUESTS_PER_CONNECTION: usize = 50;

pub async fn run_public(state: AppState, endpoint: Endpoint, handle: TaskHandle) {
    let parallel_connections_limit = Arc::new(Semaphore::new(MAX_CONNECTIONS));

    loop {
        let incoming = tokio::select! {
            incoming = endpoint.accept() => incoming,
            () = handle.make_shutdown_rx() => break,
        };

        let Some(incoming) = incoming else {
            break;
        };

        let permit = match parallel_connections_limit.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => break,
        };

        let state = state.clone();

        tokio::spawn(async move {
            let connection = match incoming.accept() {
                Ok(connecting) => match connecting.await {
                    Ok(conn) => conn,
                    Err(e) => {
                        debug!(target: LOG_GATEWAY, err = %e, "Public API: handshake failed");
                        return;
                    }
                },
                Err(e) => {
                    debug!(target: LOG_GATEWAY, err = %e, "Public API: accept failed");
                    return;
                }
            };

            if let Err(e) = handle_connection(state, connection, permit).await {
                debug!(target: LOG_GATEWAY, err = %format_args!("{e:#}"), "Public API: connection closed");
            }
        });
    }
}

async fn handle_connection(
    state: AppState,
    connection: Connection,
    _permit: tokio::sync::OwnedSemaphorePermit,
) -> anyhow::Result<()> {
    let parallel_requests_limit = Arc::new(Semaphore::new(MAX_REQUESTS_PER_CONNECTION));

    loop {
        let (send, recv) = connection.accept_bi().await?;

        let permit = parallel_requests_limit.clone().acquire_owned().await?;

        let state = state.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_request(state, send, recv, permit).await {
                warn!(target: LOG_GATEWAY, err = %format_args!("{e:#}"), "Public API: request failed");
            }
        });
    }
}

async fn handle_request(
    state: AppState,
    mut send: SendStream,
    mut recv: RecvStream,
    _permit: tokio::sync::OwnedSemaphorePermit,
) -> anyhow::Result<()> {
    let request_bytes = recv.read_to_end(GATEWAY_MAX_MESSAGE_BYTES).await?;
    let method = GatewayMethod::consensus_decode_exact(&request_bytes)?;

    let response = dispatch(state, method).await;
    let response = response.consensus_encode_to_vec();

    send.write_all(&response).await?;
    send.finish()?;

    Ok(())
}

async fn dispatch(state: AppState, method: GatewayMethod) -> Result<Vec<u8>, ApiError> {
    match method {
        GatewayMethod::GatewayInfo(req) => handle_gateway_info(state, req).await,
        GatewayMethod::SendPayment(req) => handle_send_payment(state, req).await,
        GatewayMethod::CreateBolt11Invoice(req) => handle_create_bolt11_invoice(state, req).await,
        GatewayMethod::VerifyBolt11Preimage(req) => handle_verify_bolt11_preimage(state, req).await,
    }
}

async fn handle_gateway_info(
    state: AppState,
    req: GatewayInfoRequest,
) -> Result<Vec<u8>, ApiError> {
    let gateway_info = state
        .gateway_info(&req.federation_id)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?
        .ok_or_else(|| {
            ApiError::bad_request(format!("Federation {} is not connected", req.federation_id))
        })?;

    Ok(GatewayInfoResponse { gateway_info }.consensus_encode_to_vec())
}

async fn handle_send_payment(
    state: AppState,
    req: SendPaymentRequest,
) -> Result<Vec<u8>, ApiError> {
    let outcome = state
        .send_payment(req)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;

    Ok(SendPaymentResponse { outcome }.consensus_encode_to_vec())
}

async fn handle_create_bolt11_invoice(
    state: AppState,
    req: CreateBolt11InvoiceRequest,
) -> Result<Vec<u8>, ApiError> {
    let invoice = state
        .create_bolt11_invoice(req)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;

    Ok(CreateBolt11InvoiceResponse { invoice }.consensus_encode_to_vec())
}

async fn handle_verify_bolt11_preimage(
    state: AppState,
    req: VerifyBolt11PreimageRequest,
) -> Result<Vec<u8>, ApiError> {
    let VerifyResponse { settled, preimage } = state
        .verify_bolt11_preimage(req.payment_hash, req.wait)
        .await
        .map_err(ApiError::bad_request)?;

    Ok(VerifyBolt11PreimageResponse { settled, preimage }.consensus_encode_to_vec())
}
