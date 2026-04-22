use std::collections::{BTreeMap, BTreeSet};

use crate::api::{FederationApi, FederationResult, ServerResult};
use crate::query::FilterMapThreshold;
use picomint_core::ln::contracts::IncomingContract;
use picomint_core::ln::methods::{
    AwaitIncomingContractsRequest, AwaitIncomingContractsResponse, AwaitPreimageRequest,
    AwaitPreimageResponse, ConsensusBlockCountRequest, ConsensusBlockCountResponse,
    GatewaysRequest, GatewaysResponse, LnMethod,
};
use picomint_core::module::ApiMethod;
use picomint_core::{NumPeersExt, OutPoint, PeerId};
use rand::seq::SliceRandom;

impl FederationApi {
    pub async fn ln_consensus_block_count(&self) -> FederationResult<u64> {
        self.request_current_consensus::<ConsensusBlockCountResponse>(ApiMethod::Ln(
            LnMethod::ConsensusBlockCount(ConsensusBlockCountRequest),
        ))
        .await
        .map(|resp| resp.count)
    }

    pub async fn ln_await_preimage(&self, outpoint: OutPoint, expiration: u64) -> Option<[u8; 32]> {
        self.request_current_consensus_retry::<AwaitPreimageResponse>(ApiMethod::Ln(
            LnMethod::AwaitPreimage(AwaitPreimageRequest {
                outpoint,
                expiration,
            }),
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
            .request_current_consensus_retry::<AwaitIncomingContractsResponse>(ApiMethod::Ln(
                LnMethod::AwaitIncomingContracts(AwaitIncomingContractsRequest { start, batch }),
            ))
            .await;
        (resp.contracts, resp.next_index)
    }

    pub async fn ln_gateways(&self) -> FederationResult<Vec<String>> {
        let gateways: BTreeMap<PeerId, Vec<String>> = self
            .request_with_strategy(
                FilterMapThreshold::new(
                    |_, resp: GatewaysResponse| Ok(resp.gateways),
                    self.all_peers().to_num_peers(),
                ),
                ApiMethod::Ln(LnMethod::Gateways(GatewaysRequest)),
            )
            .await?;

        let mut union = gateways
            .values()
            .flatten()
            .cloned()
            .collect::<BTreeSet<String>>()
            .into_iter()
            .collect::<Vec<String>>();

        // Shuffling the gateways ensures that payments are distributed over the
        // gateways evenly.
        union.shuffle(&mut rand::thread_rng());

        union.sort_by_cached_key(|r| {
            gateways
                .values()
                .filter(|response| !response.contains(r))
                .count()
        });

        Ok(union)
    }

    pub async fn ln_gateways_from_peer(&self, peer: PeerId) -> ServerResult<Vec<String>> {
        let resp = self
            .request_single_peer::<GatewaysResponse>(
                ApiMethod::Ln(LnMethod::Gateways(GatewaysRequest)),
                peer,
            )
            .await?;

        Ok(resp.gateways)
    }
}
