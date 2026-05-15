use crate::api::FederationApi;
use picomint_core::OutPoint;
use picomint_core::ln::ContractId;
use picomint_core::ln::methods::{
    LnMethod, OutgoingContractExpiryRequest, OutgoingContractExpiryResponse,
};
use picomint_core::module::Method;

impl FederationApi {
    pub async fn gw_outgoing_contract_expiry(
        &self,
        outpoint: OutPoint,
    ) -> anyhow::Result<Option<(ContractId, u64)>> {
        self.request_current_consensus::<OutgoingContractExpiryResponse>(Method::Ln(
            LnMethod::OutgoingContractExpiry(OutgoingContractExpiryRequest { outpoint }),
        ))
        .await
        .map(|resp| resp.contract)
    }
}
