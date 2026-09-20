//! Iroh accept loop for the broker's public API.
//!
//! Each accepted connection is served by [`picomint_rpc::handle_request`],
//! which accepts bi streams in a loop (clients keep connections alive and
//! reuse them) and handles each as one request: decode a [`BrokerMethod`],
//! dispatch to the matching `AppState` method, consensus-encode the
//! response, finish the stream.

use iroh::Endpoint;
use picomint_core::swap::methods::{BrokerMethod, InfoResponse, SwapResponse};
use picomint_encoding::Encodable as _;

use crate::AppState;

pub async fn run(state: AppState, endpoint: Endpoint) {
    picomint_rpc::run_accept_loop(endpoint, move |method| dispatch(state.clone(), method)).await;
}

async fn dispatch(state: AppState, method: BrokerMethod) -> Result<Vec<u8>, String> {
    match method {
        BrokerMethod::Info(req) => Ok(InfoResponse {
            info: state.broker_info(req.mint).ok(),
        }
        .consensus_encode_to_vec()),
        BrokerMethod::Swap(req) => state
            .swap(req)
            .await
            .map(|attestation| SwapResponse { attestation }.consensus_encode_to_vec())
            .map_err(|e| e.to_string()),
    }
}
