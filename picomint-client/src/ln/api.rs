use crate::api::FederationApi;
use picomint_core::OutPoint;
use picomint_core::ln::contracts::IncomingContractSummary;
use picomint_core::ln::gateway::GatewayPk;
use picomint_core::ln::methods::{
    AwaitIncomingContractsRequest, AwaitIncomingContractsResponse, AwaitPreimageRequest,
    AwaitPreimageResponse, GatewaysRequest, GatewaysResponse, LnMethod,
};
use picomint_core::module::Method;

pub async fn await_preimage(
    api: &FederationApi,
    outpoint: OutPoint,
    expiry: u32,
) -> Option<[u8; 32]> {
    api.request_current_consensus_retry::<AwaitPreimageResponse>(Method::Ln(
        LnMethod::AwaitPreimage(AwaitPreimageRequest { outpoint, expiry }),
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
        .request_current_consensus_retry::<AwaitIncomingContractsResponse>(Method::Ln(
            LnMethod::AwaitIncomingContracts(AwaitIncomingContractsRequest { start, batch }),
        ))
        .await;

    (resp.contracts, resp.next_index)
}

/// The federation's announced gateway list, agreed by a threshold of
/// guardians. Each guardian maintains their own vetted-gateway list
/// via the admin CLI; the response is byte-canonical (sorted via db
/// iteration) so threshold equality is deterministic.
pub async fn gateways(api: &FederationApi) -> anyhow::Result<Vec<GatewayPk>> {
    api.request_current_consensus::<GatewaysResponse>(Method::Ln(LnMethod::Gateways(
        GatewaysRequest,
    )))
    .await
    .map(|resp| resp.gateways)
}
