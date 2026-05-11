//! Freestanding API handlers for [`crate::consensus::api::ConsensusApi`].

use picomint_core::methods::{
    ConfigRequest, ConfigResponse, ExpirationStatusRequest, ExpirationStatusResponse,
    LivenessRequest, LivenessResponse, SubmitTxRequest, SubmitTxResponse,
};

use crate::consensus::api::ConsensusApi;

pub async fn submit_tx(
    api: &ConsensusApi,
    req: SubmitTxRequest,
) -> Result<SubmitTxResponse, String> {
    Ok(SubmitTxResponse {
        outcome: api.submit_tx(req.tx).await,
    })
}

pub fn config(api: &ConsensusApi, _: ConfigRequest) -> Result<ConfigResponse, String> {
    Ok(ConfigResponse {
        config: api.cfg.consensus.clone(),
    })
}

pub fn liveness(_: &ConsensusApi, _: LivenessRequest) -> Result<LivenessResponse, String> {
    Ok(LivenessResponse)
}

pub fn expiration_status(
    api: &ConsensusApi,
    _: ExpirationStatusRequest,
) -> Result<ExpirationStatusResponse, String> {
    Ok(ExpirationStatusResponse {
        status: api.expiration_status_ui(),
    })
}
