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
use picomint_encoding::Encodable as _;
use picomint_redb::ReadTx;
use tbs::BlindedSignatureShare;

use super::Mint;
use super::db::{BLINDED_SIGNATURE_SHARE, BLINDED_SIGNATURE_SHARE_RECOVERY, RECOVERY_ITEM};

pub async fn signature_shares(
    mint: &Mint,
    req: SignatureSharesRequest,
) -> Result<SignatureSharesResponse, String> {
    // Wait until any BLINDED_SIGNATURE_SHARE for this txid exists. All mint
    // outputs of a given tx are signed atomically in the same consensus
    // commit, so observing one implies all are present.
    let (shares, _dbtx) = mint
        .db
        .wait_table_check(&BLINDED_SIGNATURE_SHARE, |dbtx| {
            Some(collect_signature_shares(dbtx, req.txid)).filter(|s| !s.is_empty())
        })
        .await;

    Ok(SignatureSharesResponse { shares })
}

pub fn signature_shares_recovery(
    mint: &Mint,
    req: SignatureSharesRecoveryRequest,
) -> Result<SignatureSharesRecoveryResponse, String> {
    let mut shares = Vec::new();

    let dbtx = mint.db.begin_read();

    for message in req.messages {
        let share = dbtx
            .get(&BLINDED_SIGNATURE_SHARE_RECOVERY, &message)
            .ok_or_else(|| "No blinded signature share found".to_string())?;

        shares.push(share);
    }

    Ok(SignatureSharesRecoveryResponse { shares })
}

pub fn recovery_slice(
    mint: &Mint,
    req: RecoverySliceRequest,
) -> Result<RecoverySliceResponse, String> {
    let dbtx = mint.db.begin_read();
    Ok(RecoverySliceResponse {
        items: collect_recovery_slice(&dbtx, req.start, req.end),
    })
}

pub fn recovery_slice_hash(
    mint: &Mint,
    req: RecoverySliceHashRequest,
) -> Result<RecoverySliceHashResponse, String> {
    let dbtx = mint.db.begin_read();
    Ok(RecoverySliceHashResponse {
        hash: collect_recovery_slice(&dbtx, req.start, req.end).consensus_hash(),
    })
}

pub fn recovery_count(
    mint: &Mint,
    _: RecoveryCountRequest,
) -> Result<RecoveryCountResponse, String> {
    let dbtx = mint.db.begin_read();
    Ok(RecoveryCountResponse {
        count: super::get_recovery_count(&dbtx),
    })
}

fn collect_signature_shares(dbtx: &ReadTx, txid: TransactionId) -> Vec<BlindedSignatureShare> {
    let bounds = OutPoint { txid, out_idx: 0 }..OutPoint {
        txid,
        out_idx: u64::MAX,
    };

    dbtx.range(&BLINDED_SIGNATURE_SHARE, bounds, |r| {
        r.map(|(_, v)| v).collect()
    })
}

fn collect_recovery_slice(dbtx: &ReadTx, start: u64, end: u64) -> Vec<RecoveryItem> {
    dbtx.range(&RECOVERY_ITEM, start..end, |r| r.map(|(_, v)| v).collect())
}
