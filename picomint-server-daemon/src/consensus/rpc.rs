//! Freestanding API handlers for [`crate::consensus::api::ConsensusApi`].

use picomint_core::methods::{
    ConfigRequest, ConfigResponse, LivenessRequest, LivenessResponse, SubmitTransactionRequest,
    SubmitTransactionResponse,
};
use picomint_core::module::ApiError;

use crate::consensus::api::ConsensusApi;

pub async fn submit_transaction(
    api: &ConsensusApi,
    req: SubmitTransactionRequest,
) -> Result<SubmitTransactionResponse, ApiError> {
    Ok(SubmitTransactionResponse {
        outcome: api.submit_transaction(req.transaction).await,
    })
}

pub fn config(api: &ConsensusApi, _: ConfigRequest) -> Result<ConfigResponse, ApiError> {
    Ok(ConfigResponse {
        config: api.cfg.consensus.clone(),
    })
}

pub fn liveness(_: &ConsensusApi, _: LivenessRequest) -> Result<LivenessResponse, ApiError> {
    Ok(LivenessResponse)
}
