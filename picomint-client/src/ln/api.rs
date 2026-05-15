use crate::api::FederationApi;
use picomint_core::OutPoint;
use picomint_core::ln::contracts::IncomingContract;
use picomint_core::ln::methods::{
    AwaitIncomingContractsRequest, AwaitIncomingContractsResponse, AwaitPreimageRequest,
    AwaitPreimageResponse, ConsensusBlockCountRequest, ConsensusBlockCountResponse,
    GatewaysRequest, GatewaysResponse, LnMethod,
};
use picomint_core::module::Method;

impl FederationApi {
    pub async fn ln_consensus_block_count(&self) -> anyhow::Result<u64> {
        self.request_current_consensus::<ConsensusBlockCountResponse>(Method::Ln(
            LnMethod::ConsensusBlockCount(ConsensusBlockCountRequest),
        ))
        .await
        .map(|resp| resp.count)
    }

    pub async fn ln_await_preimage(&self, outpoint: OutPoint, expiry: u64) -> Option<[u8; 32]> {
        self.request_current_consensus_retry::<AwaitPreimageResponse>(Method::Ln(
            LnMethod::AwaitPreimage(AwaitPreimageRequest { outpoint, expiry }),
        ))
        .await
        .preimage
    }

    pub async fn ln_await_incoming_contracts(
        &self,
        start: u64,
        batch: u64,
    ) -> (Vec<(OutPoint, IncomingContract)>, u64) {
        let resp = self
            .request_current_consensus_retry::<AwaitIncomingContractsResponse>(Method::Ln(
                LnMethod::AwaitIncomingContracts(AwaitIncomingContractsRequest { start, batch }),
            ))
            .await;
        (resp.contracts, resp.next_index)
    }

    /// The federation's announced gateway list, agreed by a threshold of
    /// guardians. Each guardian maintains their own vetted-gateway list
    /// via the admin CLI; the response is byte-canonical (sorted via redb
    /// iteration) so threshold equality is deterministic.
    pub async fn ln_gateways(&self) -> anyhow::Result<Vec<picomint_core::ln::gateway::GatewayPk>> {
        self.request_current_consensus::<GatewaysResponse>(Method::Ln(LnMethod::Gateways(
            GatewaysRequest,
        )))
        .await
        .map(|resp| resp.gateways)
    }
}
