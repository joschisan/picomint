use std::collections::BTreeMap;
use std::sync::Arc;

use crate::api::MintApi;
use picomint_core::ecash::Denomination;
use picomint_core::ecash::methods::{
    EcashMethod, IssuanceStateRequest, IssuanceStateResponse, SignatureSharesRequest,
    SignatureSharesResponse, SignatureSharesRestoreRequest, SignatureSharesRestoreResponse,
    SpendStateRequest, SpendStateResponse,
};
use picomint_core::methods::Method;
use picomint_core::secp256k1::XOnlyPublicKey;
use picomint_core::{NodeId, TransactionId};
use picomint_rpc::query::FilterMapThreshold;
use tbs::{BlindedNonce, BlindedSignatureShare, PublicKeyShare};

use super::NoteIssuanceRequest;
use super::ecash_sm::verify_blind_shares;

pub async fn signature_shares(
    api: &MintApi,
    txid: TransactionId,
    issuance_requests: Vec<NoteIssuanceRequest>,
    tbs_pks: BTreeMap<Denomination, BTreeMap<NodeId, PublicKeyShare>>,
) -> BTreeMap<NodeId, Vec<BlindedSignatureShare>> {
    let issuance_requests = Arc::new(issuance_requests);
    let tbs_pks = Arc::new(tbs_pks);

    api.request_with_strategy_retry(
        FilterMapThreshold::new(
            move |node, resp: SignatureSharesResponse| {
                verify_blind_shares_blocking(
                    node,
                    resp.shares,
                    issuance_requests.clone(),
                    tbs_pks.clone(),
                )
            },
            api.num_nodes(),
        ),
        Method::Ecash(EcashMethod::SignatureShares(SignatureSharesRequest {
            txid,
        })),
    )
    .await
}

/// [`verify_blind_shares`] on the blocking pool: a pairing per share,
/// which would otherwise pin a runtime worker for the whole batch.
async fn verify_blind_shares_blocking(
    node: NodeId,
    shares: Vec<BlindedSignatureShare>,
    issuance_requests: Arc<Vec<NoteIssuanceRequest>>,
    tbs_pks: Arc<BTreeMap<Denomination, BTreeMap<NodeId, PublicKeyShare>>>,
) -> anyhow::Result<Vec<BlindedSignatureShare>> {
    tokio::task::spawn_blocking(move || {
        verify_blind_shares(node, shares, &issuance_requests, &tbs_pks)
    })
    .await
    .expect("Share verification cannot panic")
}

/// Fetch shares for notes a restore scan has already established the
/// mint signed. Every nonce must resolve on every node, so a
/// candidate can never be silently dropped for want of a full column of
/// shares to interpolate over.
pub async fn signature_shares_restore(
    api: &MintApi,
    issuance_requests: Vec<NoteIssuanceRequest>,
    tbs_pks: BTreeMap<Denomination, BTreeMap<NodeId, PublicKeyShare>>,
) -> BTreeMap<NodeId, Vec<BlindedSignatureShare>> {
    let nonces = issuance_requests
        .iter()
        .map(NoteIssuanceRequest::blinded_nonce)
        .collect();

    let issuance_requests = Arc::new(issuance_requests);
    let tbs_pks = Arc::new(tbs_pks);

    api.request_with_strategy_retry(
        FilterMapThreshold::new(
            move |node, resp: SignatureSharesRestoreResponse| {
                verify_blind_shares_blocking(
                    node,
                    resp.shares,
                    issuance_requests.clone(),
                    tbs_pks.clone(),
                )
            },
            api.num_nodes(),
        ),
        Method::Ecash(EcashMethod::SignatureSharesRestore(
            SignatureSharesRestoreRequest { nonces },
        )),
    )
    .await
}

/// Which of `nonces` the mint has already seen spent, and which of
/// `nonces` it ever signed. Both go through threshold consensus rather
/// than a single node: either answer coming back wrong in the negative
/// direction makes a restoring wallet abandon a live note, so a lone
/// node must not be able to decide it.
pub async fn spend_state(api: &MintApi, nonces: Vec<XOnlyPublicKey>) -> Vec<bool> {
    api.request_current_consensus_retry::<SpendStateResponse>(Method::Ecash(
        EcashMethod::SpendState(SpendStateRequest { nonces }),
    ))
    .await
    .spent
}

/// For each nonce, the denomination the mint signed it under, or
/// `None` if it never did. The denomination is not derivable from the
/// seed under a single counter space, so the scan takes the mint's
/// word for it — and then checks that word when it aggregates the share.
pub async fn issuance_state(api: &MintApi, nonces: Vec<BlindedNonce>) -> Vec<Option<Denomination>> {
    api.request_current_consensus_retry::<IssuanceStateResponse>(Method::Ecash(
        EcashMethod::IssuanceState(IssuanceStateRequest { nonces }),
    ))
    .await
    .issued
}
