use crate::api::FederationApi;
use picomint_core::OutPoint;
use picomint_core::ln::ContractId;
use picomint_core::ln::methods::{
    LnMethod, OutgoingContractExpiryRequest, OutgoingContractExpiryResponse,
};
use picomint_core::module::Method;

/// The contract id and expiry of a confirmed outgoing contract, or `None`
/// while it is unconfirmed. Called by the gateway daemon to validate a
/// send request against the federation before paying its invoice.
pub async fn outgoing_contract_expiry(
    api: &FederationApi,
    outpoint: OutPoint,
) -> anyhow::Result<Option<(ContractId, u32)>> {
    api.request_current_consensus::<OutgoingContractExpiryResponse>(Method::Ln(
        LnMethod::OutgoingContractExpiry(OutgoingContractExpiryRequest { outpoint }),
    ))
    .await
    .map(|resp| resp.contract)
}
