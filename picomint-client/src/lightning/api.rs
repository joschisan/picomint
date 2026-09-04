use crate::api::FederationApi;
use picomint_core::OutPoint;
use picomint_core::lightning::contracts::IncomingContractSummary;
use picomint_core::lightning::gateway::GatewayPk;
use picomint_core::lightning::methods::{
    AwaitIncomingContractsRequest, AwaitIncomingContractsResponse, AwaitPreimageRequest,
    AwaitPreimageResponse, GatewaysRequest, GatewaysResponse, LightningMethod,
};
use picomint_core::module::Method;

pub async fn await_preimage(
    api: &FederationApi,
    outpoint: OutPoint,
    expiry: u32,
) -> Option<[u8; 32]> {
    api.request_current_consensus_retry::<AwaitPreimageResponse>(Method::Lightning(
        LightningMethod::AwaitPreimage(AwaitPreimageRequest { outpoint, expiry }),
    ))
    .await
    .preimage
}

pub async fn await_incoming_contracts(
    api: &FederationApi,
    start: u64,
    batch: u64,
) -> (Vec<IncomingContractSummary>, u64) {
    let resp = api
        .request_current_consensus_retry::<AwaitIncomingContractsResponse>(Method::Lightning(
            LightningMethod::AwaitIncomingContracts(AwaitIncomingContractsRequest { start, batch }),
        ))
        .await;

    (resp.contracts, resp.next_index)
}

/// The federation's announced gateway list, agreed by a threshold of
/// guardians. Each guardian maintains their own vetted-gateway list
/// via the admin CLI; the response is byte-canonical (sorted via db
/// iteration) so threshold equality is deterministic.
pub async fn gateways(api: &FederationApi) -> anyhow::Result<Vec<GatewayPk>> {
    api.request_current_consensus::<GatewaysResponse>(Method::Lightning(LightningMethod::Gateways(
        GatewaysRequest,
    )))
    .await
    .map(|resp| resp.gateways)
}
