//! Iroh accept loop for the gateway's public API.
//!
//! Each accepted connection runs through the one-shot RPC lifecycle from
//! [`picomint_rpc::handle_request`]: accept one bi stream, decode a
//! [`GatewayMethod`], dispatch to the matching `AppState` method,
//! consensus-encode the response, finish the stream, await the
//! client-driven close.

use iroh::Endpoint;
use picomint_core::ln::gateway_api::{
    CreateInvoiceResponse, GatewayMethod, InfoResponse, SendPaymentResponse,
};
use picomint_encoding::Encodable as _;

use crate::AppState;

/// Maximum number of concurrent in-flight gateway API requests, summed
/// across every accepted connection.
pub const MAX_CONCURRENT_REQUESTS: usize = 1000;

pub async fn run_public(state: AppState, endpoint: Endpoint) {
    picomint_rpc::run_accept_loop(endpoint, MAX_CONCURRENT_REQUESTS, move |method| {
        dispatch(state.clone(), method)
    })
    .await;
}

async fn dispatch(state: AppState, method: GatewayMethod) -> Result<Vec<u8>, String> {
    match method {
        GatewayMethod::Info(req) => Ok(InfoResponse {
            info: state.gateway_info(&req.federation_id).await.ok(),
        }
        .consensus_encode_to_vec()),
        GatewayMethod::SendPayment(req) => state
            .send_payment(req)
            .await
            .map(|result| SendPaymentResponse { result }.consensus_encode_to_vec())
            .map_err(|e| e.to_string()),
        GatewayMethod::CreateInvoice(req) => state
            .create_bolt11_invoice(req)
            .await
            .map(|invoice| CreateInvoiceResponse { invoice }.consensus_encode_to_vec())
            .map_err(|e| e.to_string()),
        GatewayMethod::VerifyPreimage(req) => state
            .verify_bolt11_preimage(req.hash, req.wait)
            .await
            .map(|resp| resp.consensus_encode_to_vec())
            .map_err(|e| e.to_string()),
    }
}
