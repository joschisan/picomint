use crate::api::MintApi;
use picomint_core::OutPoint;
use picomint_core::lightning::ContractId;
use picomint_core::lightning::methods::{
    AwaitOutgoingContractRequest, AwaitOutgoingContractResponse, LightningMethod,
};
use picomint_core::methods::Method;

/// The contract id of an outgoing contract, once the mint holds it. Called
/// by the gateway daemon to validate a send request against the mint
/// before paying its invoice.
pub async fn await_outgoing_contract(
    api: &MintApi,
    outpoint: OutPoint,
) -> anyhow::Result<ContractId> {
    api.request_current_consensus::<AwaitOutgoingContractResponse>(Method::Lightning(
        LightningMethod::AwaitOutgoingContract(AwaitOutgoingContractRequest { outpoint }),
    ))
    .await
    .map(|resp| resp.contract)
}
