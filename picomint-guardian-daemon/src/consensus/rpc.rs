//! Freestanding API handlers for [`crate::consensus::api::ConsensusApi`].

use picomint_core::methods::{
    ConfigRequest, ConfigResponse, ExpiryStatusRequest, ExpiryStatusResponse, LivenessRequest,
    LivenessResponse, SubmitTxRequest, SubmitTxResponse,
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

pub fn config(api: &ConsensusApi, req: ConfigRequest) -> Result<ConfigResponse, String> {
    api.register_config_download(req.invite_id)?;

    Ok(ConfigResponse {
        config: api.cfg.consensus.clone(),
    })
}

pub fn liveness(_: &ConsensusApi, _: LivenessRequest) -> Result<LivenessResponse, String> {
    Ok(LivenessResponse)
}

pub fn expiry_status(
    api: &ConsensusApi,
    _: ExpiryStatusRequest,
) -> Result<ExpiryStatusResponse, String> {
    Ok(ExpiryStatusResponse {
        status: api.expiry_status_ui(),
    })
}
