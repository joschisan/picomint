use crate::api::MintApi;
use picomint_core::OutPoint;
use picomint_core::methods::Method;
use picomint_core::swap::broker::BrokerPk;
use picomint_core::swap::methods::{
    AwaitReceiveContractsRequest, AwaitReceiveContractsResponse, BrokersRequest, BrokersResponse,
    SendContractRequest, SendContractResponse, SwapMethod,
};
use picomint_core::swap::{ReceiveContractSummary, SendContract};

pub async fn await_receive_contracts(
    api: &MintApi,
    start: u64,
    batch: u64,
) -> (Vec<ReceiveContractSummary>, u64) {
    let resp = api
        .request_current_consensus_retry::<AwaitReceiveContractsResponse>(Method::Swap(
            SwapMethod::AwaitReceiveContracts(AwaitReceiveContractsRequest { start, batch }),
        ))
        .await;

    (resp.contracts, resp.next_index)
}

/// The mint's announced broker list, agreed by a threshold of nodes. Each
/// node maintains its own vetted-broker list via the admin CLI; the
/// response is byte-canonical (sorted via db iteration) so threshold
/// equality is deterministic.
pub async fn brokers(api: &MintApi) -> anyhow::Result<Vec<BrokerPk>> {
    api.request_current_consensus::<BrokersResponse>(Method::Swap(SwapMethod::Brokers(
        BrokersRequest,
    )))
    .await
    .map(|resp| resp.brokers)
}

/// The send contract at `outpoint`, once the mint holds it. Called by the
/// broker daemon to validate a swap request against the mint before
/// funding the receive contract.
pub async fn send_contract(api: &MintApi, outpoint: OutPoint) -> anyhow::Result<SendContract> {
    api.request_current_consensus::<SendContractResponse>(Method::Swap(SwapMethod::SendContract(
        SendContractRequest { outpoint },
    )))
    .await
    .map(|resp| resp.contract)
}
