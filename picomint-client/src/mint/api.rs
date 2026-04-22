use std::collections::BTreeMap;
use std::time::Duration;

use crate::api::{FederationApi, ServerError};
use crate::query::FilterMapThreshold;
use bitcoin_hashes::sha256;
use picomint_core::mint::methods::{
    MintMethod, RecoveryCountRequest, RecoveryCountResponse, RecoverySliceHashRequest,
    RecoverySliceHashResponse, RecoverySliceRequest, RecoverySliceResponse,
    SignatureSharesRecoveryRequest, SignatureSharesRecoveryResponse, SignatureSharesRequest,
    SignatureSharesResponse,
};
use picomint_core::mint::{Denomination, RecoveryItem};
use picomint_core::module::Method;
use picomint_core::{NumPeersExt, PeerId, TransactionId};
use tbs::{BlindedMessage, BlindedSignatureShare, PublicKeyShare};

use super::NoteIssuanceRequest;
use super::issuance_sm::verify_blind_shares;

impl FederationApi {
    pub async fn signature_shares(
        &self,
        txid: TransactionId,
        issuance_requests: Vec<NoteIssuanceRequest>,
        tbs_pks: BTreeMap<Denomination, BTreeMap<PeerId, PublicKeyShare>>,
    ) -> BTreeMap<PeerId, Vec<BlindedSignatureShare>> {
        self.request_with_strategy_retry(
            FilterMapThreshold::new(
                move |peer, resp: SignatureSharesResponse| {
                    verify_blind_shares(peer, resp.shares, &issuance_requests, &tbs_pks)
                        .map_err(ServerError::InvalidResponse)
                },
                self.all_peers().to_num_peers(),
            ),
            Method::Mint(MintMethod::SignatureShares(SignatureSharesRequest { txid })),
        )
        .await
    }

    pub async fn signature_shares_recovery(
        &self,
        issuance_requests: Vec<NoteIssuanceRequest>,
        tbs_pks: BTreeMap<Denomination, BTreeMap<PeerId, PublicKeyShare>>,
    ) -> BTreeMap<PeerId, Vec<BlindedSignatureShare>> {
        let messages: Vec<BlindedMessage> = issuance_requests
            .iter()
            .map(NoteIssuanceRequest::blinded_message)
            .collect();

        self.request_with_strategy_retry(
            FilterMapThreshold::new(
                move |peer, resp: SignatureSharesRecoveryResponse| {
                    verify_blind_shares(peer, resp.shares, &issuance_requests, &tbs_pks)
                        .map_err(ServerError::InvalidResponse)
                },
                self.all_peers().to_num_peers(),
            ),
            Method::Mint(MintMethod::SignatureSharesRecovery(
                SignatureSharesRecoveryRequest { messages },
            )),
        )
        .await
    }

    pub async fn recovery_count(&self) -> anyhow::Result<u64> {
        self.request_current_consensus::<RecoveryCountResponse>(Method::Mint(
            MintMethod::RecoveryCount(RecoveryCountRequest),
        ))
        .await
        .map(|resp| resp.count)
        .map_err(|_| anyhow::anyhow!("Failed to request recovery count"))
    }

    pub async fn recovery_slice_hash(&self, start: u64, end: u64) -> sha256::Hash {
        self.request_current_consensus_retry::<RecoverySliceHashResponse>(Method::Mint(
            MintMethod::RecoverySliceHash(RecoverySliceHashRequest { start, end }),
        ))
        .await
        .hash
    }

    pub async fn recovery_slice(
        &self,
        peer: PeerId,
        timeout: Duration,
        start: u64,
        end: u64,
    ) -> anyhow::Result<Vec<RecoveryItem>> {
        let result: RecoverySliceResponse = tokio::time::timeout(
            timeout,
            self.request_single_peer::<RecoverySliceResponse>(
                Method::Mint(MintMethod::RecoverySlice(RecoverySliceRequest {
                    start,
                    end,
                })),
                peer,
            ),
        )
        .await??;

        Ok(result.items)
    }
}
