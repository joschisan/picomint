use std::collections::BTreeMap;

use crate::api::MintApi;
use picomint_core::ecash::Denomination;
use picomint_core::ecash::methods::{
    IssuanceStateRequest, IssuanceStateResponse, ECashMethod, SignaturesRequest, SignaturesResponse,
    SignaturesRestoreRequest, SignaturesRestoreResponse, SpendStateRequest, SpendStateResponse,
};
use picomint_core::module::Method;
use picomint_core::secp256k1::XOnlyPublicKey;
use picomint_core::{NodeId, TransactionId};
use picomint_rpc::query::FilterMapThreshold;
use tbs::{BlindedMessage, BlindedSignatureShare, PublicKeyShare};

use super::NoteIssuanceRequest;
use super::ecash_sm::verify_blind_shares;

pub async fn signatures(
    api: &MintApi,
    txid: TransactionId,
    issuance_requests: Vec<NoteIssuanceRequest>,
    tbs_pks: BTreeMap<Denomination, BTreeMap<NodeId, PublicKeyShare>>,
) -> BTreeMap<NodeId, Vec<BlindedSignatureShare>> {
    api.request_with_strategy_retry(
        FilterMapThreshold::new(
            move |node, resp: SignaturesResponse| {
                verify_blind_shares(node, resp.shares, &issuance_requests, &tbs_pks)
            },
            api.num_peers(),
        ),
        Method::ECash(ECashMethod::Signatures(SignaturesRequest { txid })),
    )
    .await
}

/// Fetch shares for notes a restore scan has already established the
/// mint signed. Every message must resolve on every node, so a
/// candidate can never be silently dropped for want of a full column of
/// shares to interpolate over.
pub async fn signatures_restore(
    api: &MintApi,
    issuance_requests: Vec<NoteIssuanceRequest>,
    tbs_pks: BTreeMap<Denomination, BTreeMap<NodeId, PublicKeyShare>>,
) -> BTreeMap<NodeId, Vec<BlindedSignatureShare>> {
    let messages = issuance_requests
        .iter()
        .map(NoteIssuanceRequest::blinded_message)
        .collect();

    api.request_with_strategy_retry(
        FilterMapThreshold::new(
            move |node, resp: SignaturesRestoreResponse| {
                verify_blind_shares(node, resp.shares, &issuance_requests, &tbs_pks)
            },
            api.num_peers(),
        ),
        Method::ECash(ECashMethod::SignaturesRestore(SignaturesRestoreRequest {
            messages,
        })),
    )
    .await
}

/// Which of `nonces` the mint has already seen spent, and which of
/// `messages` it ever signed. Both go through threshold consensus rather
/// than a single node: either answer coming back wrong in the negative
/// direction makes a restoring wallet abandon a live note, so a lone
/// node must not be able to decide it.
pub async fn spend_state(api: &MintApi, nonces: Vec<XOnlyPublicKey>) -> Vec<bool> {
    api.request_current_consensus_retry::<SpendStateResponse>(Method::ECash(ECashMethod::SpendState(
        SpendStateRequest { nonces },
    )))
    .await
    .spent
}

/// For each message, the denomination the mint signed it under, or
/// `None` if it never did. The denomination is not derivable from the
/// seed under a single counter space, so the scan takes the mint's
/// word for it — and then checks that word when it aggregates the share.
pub async fn issuance_state(
    api: &MintApi,
    messages: Vec<BlindedMessage>,
) -> Vec<Option<Denomination>> {
    api.request_current_consensus_retry::<IssuanceStateResponse>(Method::ECash(
        ECashMethod::IssuanceState(IssuanceStateRequest { messages }),
    ))
    .await
    .issued
}
