use crate::api::{FederationApi, FederationResult};
use picomint_core::OutPoint;
use picomint_core::ln::ContractId;
use picomint_core::ln::methods::METHOD_OUTGOING_CONTRACT_EXPIRATION;
use picomint_core::module::ApiRequestErased;

impl FederationApi {
    pub async fn gw_outgoing_contract_expiration(
        &self,
        outpoint: OutPoint,
    ) -> FederationResult<Option<(ContractId, u64)>> {
        self.request_current_consensus(
            METHOD_OUTGOING_CONTRACT_EXPIRATION.to_string(),
            ApiRequestErased::new(outpoint),
        )
        .await
    }
}
