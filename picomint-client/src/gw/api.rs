use crate::api::{FederationApi, FederationResult};
use picomint_core::OutPoint;
use picomint_core::ln::ContractId;
use picomint_core::ln::methods::{
    LnMethod, OutgoingContractExpirationRequest, OutgoingContractExpirationResponse,
};
use picomint_core::module::Method;

impl FederationApi {
    pub async fn gw_outgoing_contract_expiration(
        &self,
        outpoint: OutPoint,
    ) -> FederationResult<Option<(ContractId, u64)>> {
        self.request_current_consensus::<OutgoingContractExpirationResponse>(Method::Ln(
            LnMethod::OutgoingContractExpiration(OutgoingContractExpirationRequest { outpoint }),
        ))
        .await
        .map(|resp| resp.contract)
    }
}
