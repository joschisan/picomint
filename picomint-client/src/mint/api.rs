use std::collections::BTreeMap;

use crate::api::FederationApi;
use crate::query::FilterMapThreshold;
use picomint_core::mint::Denomination;
use picomint_core::mint::methods::{
    IssuanceStateRequest, IssuanceStateResponse, MintMethod, SignatureSharesRequest,
    SignatureSharesResponse, SignatureSharesRestoreRequest, SignatureSharesRestoreResponse,
    SpendStateRequest, SpendStateResponse,
};
use picomint_core::module::Method;
use picomint_core::secp256k1::XOnlyPublicKey;
use picomint_core::{PeerId, TransactionId};
use tbs::{BlindedMessage, BlindedSignatureShare, PublicKeyShare};

use super::NoteIssuanceRequest;
use super::mint_sm::verify_blind_shares;

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
                },
                self.num_peers(),
            ),
            Method::Mint(MintMethod::SignatureShares(SignatureSharesRequest { txid })),
        )
        .await
    }

    /// Fetch shares for notes a restore scan has already established the
    /// federation signed. Every message must resolve on every peer, so a
    /// candidate can never be silently dropped for want of a full column of
    /// shares to interpolate over.
    pub async fn signature_shares_restore(
        &self,
        issuance_requests: Vec<NoteIssuanceRequest>,
        tbs_pks: BTreeMap<Denomination, BTreeMap<PeerId, PublicKeyShare>>,
    ) -> BTreeMap<PeerId, Vec<BlindedSignatureShare>> {
        let messages = issuance_requests
            .iter()
            .map(NoteIssuanceRequest::blinded_message)
            .collect();

        self.request_with_strategy_retry(
            FilterMapThreshold::new(
                move |peer, resp: SignatureSharesRestoreResponse| {
                    verify_blind_shares(peer, resp.shares, &issuance_requests, &tbs_pks)
                },
                self.num_peers(),
            ),
            Method::Mint(MintMethod::SignatureSharesRestore(
                SignatureSharesRestoreRequest { messages },
            )),
        )
        .await
    }

    /// Which of `nonces` the federation has already seen spent, and which of
    /// `messages` it ever signed. Both go through threshold consensus rather
    /// than a single peer: either answer coming back wrong in the negative
    /// direction makes a restoring wallet abandon a live note, so a lone
    /// peer must not be able to decide it.
    pub async fn spend_state(&self, nonces: Vec<XOnlyPublicKey>) -> Vec<bool> {
        self.request_current_consensus_retry::<SpendStateResponse>(Method::Mint(
            MintMethod::SpendState(SpendStateRequest { nonces }),
        ))
        .await
        .spent
    }

    pub async fn issuance_state(&self, messages: Vec<BlindedMessage>) -> Vec<bool> {
        self.request_current_consensus_retry::<IssuanceStateResponse>(Method::Mint(
            MintMethod::IssuanceState(IssuanceStateRequest { messages }),
        ))
        .await
        .issued
    }
}
