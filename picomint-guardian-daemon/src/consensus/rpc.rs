//! Freestanding API handlers for [`crate::consensus::api::ConsensusApi`].

use picomint_core::methods::{
    ConfigRequest, ConfigResponse, ExpiryStatusRequest, ExpiryStatusResponse,
    FederationInfoRequest, FederationInfoResponse, LivenessRequest, LivenessResponse,
    SubmitTxRequest, SubmitTxResponse,
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
        config: api.server.cfg.consensus.clone(),
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

/// Ungated, unlike [`config`]: the federation id and peer set are already held
/// by any joined client, and a caller that got them out of band can pin them
/// against a hash. Serving them grants nothing an invite would otherwise gate.
pub fn federation_info(
    api: &ConsensusApi,
    _: FederationInfoRequest,
) -> Result<FederationInfoResponse, String> {
    Ok(FederationInfoResponse::new(&api.server.cfg.consensus))
}
