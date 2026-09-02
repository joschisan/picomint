use std::collections::BTreeMap;

use crate::api::FederationApi;
use picomint_core::mint::Denomination;
use picomint_core::mint::methods::{
    IssuanceStateRequest, IssuanceStateResponse, MintMethod, SignaturesRequest, SignaturesResponse,
    SignaturesRestoreRequest, SignaturesRestoreResponse, SpendStateRequest, SpendStateResponse,
};
use picomint_core::module::Method;
use picomint_core::secp256k1::XOnlyPublicKey;
use picomint_core::{PeerId, TransactionId};
use picomint_rpc::query::FilterMapThreshold;
use tbs::{BlindedMessage, BlindedSignatureShare, PublicKeyShare};

use super::NoteIssuanceRequest;
use super::mint_sm::verify_blind_shares;

pub async fn signatures(
    api: &FederationApi,
    txid: TransactionId,
    issuance_requests: Vec<NoteIssuanceRequest>,
    tbs_pks: BTreeMap<Denomination, BTreeMap<PeerId, PublicKeyShare>>,
) -> BTreeMap<PeerId, Vec<BlindedSignatureShare>> {
    api.request_with_strategy_retry(
        FilterMapThreshold::new(
            move |peer, resp: SignaturesResponse| {
                verify_blind_shares(peer, resp.shares, &issuance_requests, &tbs_pks)
            },
            api.num_peers(),
        ),
        Method::Mint(MintMethod::Signatures(SignaturesRequest { txid })),
    )
    .await
}

/// Fetch shares for notes a restore scan has already established the
/// federation signed. Every message must resolve on every peer, so a
/// candidate can never be silently dropped for want of a full column of
/// shares to interpolate over.
pub async fn signatures_restore(
    api: &FederationApi,
    issuance_requests: Vec<NoteIssuanceRequest>,
    tbs_pks: BTreeMap<Denomination, BTreeMap<PeerId, PublicKeyShare>>,
) -> BTreeMap<PeerId, Vec<BlindedSignatureShare>> {
    let messages = issuance_requests
        .iter()
        .map(NoteIssuanceRequest::blinded_message)
        .collect();

    api.request_with_strategy_retry(
        FilterMapThreshold::new(
            move |peer, resp: SignaturesRestoreResponse| {
                verify_blind_shares(peer, resp.shares, &issuance_requests, &tbs_pks)
            },
            api.num_peers(),
        ),
        Method::Mint(MintMethod::SignaturesRestore(SignaturesRestoreRequest {
            messages,
        })),
    )
    .await
}

/// Which of `nonces` the federation has already seen spent, and which of
/// `messages` it ever signed. Both go through threshold consensus rather
/// than a single peer: either answer coming back wrong in the negative
/// direction makes a restoring wallet abandon a live note, so a lone
/// peer must not be able to decide it.
pub async fn spend_state(api: &FederationApi, nonces: Vec<XOnlyPublicKey>) -> Vec<bool> {
    api.request_current_consensus_retry::<SpendStateResponse>(Method::Mint(MintMethod::SpendState(
        SpendStateRequest { nonces },
    )))
    .await
    .spent
}

/// For each message, the denomination the federation signed it under, or
/// `None` if it never did. The denomination is not derivable from the
/// seed under a single counter space, so the scan takes the federation's
/// word for it — and then checks that word when it aggregates the share.
pub async fn issuance_state(
    api: &FederationApi,
    messages: Vec<BlindedMessage>,
) -> Vec<Option<Denomination>> {
    api.request_current_consensus_retry::<IssuanceStateResponse>(Method::Mint(
        MintMethod::IssuanceState(IssuanceStateRequest { messages }),
    ))
    .await
    .issued
}
