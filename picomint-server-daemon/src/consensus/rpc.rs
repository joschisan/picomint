//! Freestanding API handlers for [`crate::consensus::api::ConsensusApi`].

use picomint_core::methods::{
    ConfigRequest, ConfigResponse, LivenessRequest, LivenessResponse, SubmitTxRequest,
    SubmitTxResponse,
};
use picomint_core::module::ApiError;

use crate::consensus::api::ConsensusApi;

pub async fn submit_tx(
    api: &ConsensusApi,
    req: SubmitTxRequest,
) -> Result<SubmitTxResponse, ApiError> {
    Ok(SubmitTxResponse {
        outcome: api.submit_tx(req.tx).await,
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
