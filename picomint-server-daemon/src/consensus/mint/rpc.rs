//! Freestanding API handlers for [`super::Mint`].

use picomint_core::OutPoint;
use picomint_core::TransactionId;
use picomint_core::mint::RecoveryItem;
use picomint_core::mint::methods::{
    RecoveryCountRequest, RecoveryCountResponse, RecoverySliceHashRequest,
    RecoverySliceHashResponse, RecoverySliceRequest, RecoverySliceResponse,
    SignatureSharesRecoveryRequest, SignatureSharesRecoveryResponse, SignatureSharesRequest,
    SignatureSharesResponse,
};
use picomint_core::module::ApiError;
use picomint_encoding::Encodable as _;
use picomint_redb::ReadTransaction;
use tbs::BlindedSignatureShare;

use super::Mint;
use super::db::{BLINDED_SIGNATURE_SHARE, BLINDED_SIGNATURE_SHARE_RECOVERY, RECOVERY_ITEM};

pub async fn signature_shares(
    mint: &Mint,
    req: SignatureSharesRequest,
) -> Result<SignatureSharesResponse, ApiError> {
    // Wait until any BLINDED_SIGNATURE_SHARE for this txid exists. All mint
    // outputs of a given tx are signed atomically in the same consensus
    // commit, so observing one implies all are present.
    let (shares, _tx) = mint
        .db
        .wait_table_check(&BLINDED_SIGNATURE_SHARE, |tx| {
            Some(collect_signature_shares(tx, req.txid)).filter(|s| !s.is_empty())
        })
        .await;

    Ok(SignatureSharesResponse { shares })
}

pub fn signature_shares_recovery(
    mint: &Mint,
    req: SignatureSharesRecoveryRequest,
) -> Result<SignatureSharesRecoveryResponse, ApiError> {
    let mut shares = Vec::new();

    let tx = mint.db.begin_read();

    for message in req.messages {
        let share = tx
            .get(&BLINDED_SIGNATURE_SHARE_RECOVERY, &message)
            .ok_or_else(|| ApiError::bad_request("No blinded signature share found".to_string()))?;

        shares.push(share);
    }

    Ok(SignatureSharesRecoveryResponse { shares })
}

pub fn recovery_slice(
    mint: &Mint,
    req: RecoverySliceRequest,
) -> Result<RecoverySliceResponse, ApiError> {
    let tx = mint.db.begin_read();
    Ok(RecoverySliceResponse {
        items: collect_recovery_slice(&tx, req.start, req.end),
    })
}

pub fn recovery_slice_hash(
    mint: &Mint,
    req: RecoverySliceHashRequest,
) -> Result<RecoverySliceHashResponse, ApiError> {
    let tx = mint.db.begin_read();
    Ok(RecoverySliceHashResponse {
        hash: collect_recovery_slice(&tx, req.start, req.end).consensus_hash(),
    })
}

pub fn recovery_count(
    mint: &Mint,
    _: RecoveryCountRequest,
) -> Result<RecoveryCountResponse, ApiError> {
    let tx = mint.db.begin_read();
    Ok(RecoveryCountResponse {
        count: super::get_recovery_count(&tx),
    })
}

fn collect_signature_shares(
    tx: &ReadTransaction,
    txid: TransactionId,
) -> Vec<BlindedSignatureShare> {
    let bounds = OutPoint { txid, out_idx: 0 }..OutPoint {
        txid,
        out_idx: u64::MAX,
    };

    tx.range(&BLINDED_SIGNATURE_SHARE, bounds, |r| {
        r.map(|(_, v)| v).collect()
    })
}

fn collect_recovery_slice(tx: &ReadTransaction, start: u64, end: u64) -> Vec<RecoveryItem> {
    tx.range(&RECOVERY_ITEM, start..end, |r| r.map(|(_, v)| v).collect())
}
