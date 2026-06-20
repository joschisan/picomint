//! Iroh accept loop for the gateway's public API.
//!
//! Each accepted connection is served by [`picomint_rpc::handle_request`],
//! which accepts bi streams in a loop (clients keep connections alive and
//! reuse them) and handles each as one request: decode a [`GatewayMethod`],
//! dispatch to the matching `AppState` method, consensus-encode the
//! response, finish the stream.

use iroh::Endpoint;
use picomint_core::ln::methods::{GatewayMethod, InfoResponse, ReceiveResponse, SendResponse};
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
            info: state.gateway_info(&req.federation).await.ok(),
        }
        .consensus_encode_to_vec()),
        GatewayMethod::Send(req) => state
            .send(req)
            .await
            .map(|result| SendResponse { result }.consensus_encode_to_vec())
            .map_err(|e| e.to_string()),
        GatewayMethod::Receive(req) => state
            .receive(req)
            .await
            .map(|invoice| ReceiveResponse { invoice }.consensus_encode_to_vec())
            .map_err(|e| e.to_string()),
        GatewayMethod::Verify(req) => state
            .verify(req.hash, req.wait)
            .await
            .map(|resp| resp.consensus_encode_to_vec())
            .map_err(|e| e.to_string()),
    }
}
