//! Freestanding API handlers for [`super::Mint`].

use picomint_core::OutPoint;
use picomint_core::TransactionId;
use picomint_core::mint::methods::{
    IssuanceStateRequest, IssuanceStateResponse, SignatureSharesRequest, SignatureSharesResponse,
    SignatureSharesRestoreRequest, SignatureSharesRestoreResponse, SpendStateRequest,
    SpendStateResponse,
};
use picomint_sqlite::ReadTx;
use tbs::BlindedSignatureShare;

use super::Mint;
use super::db::{
    BlindedNonceTable, BlindedSignatureShareRestoreTable, BlindedSignatureShareTable,
    NoteNonceTable,
};

pub async fn signature_shares(
    mint: &Mint,
    req: SignatureSharesRequest,
) -> Result<SignatureSharesResponse, String> {
    // Wait until any BlindedSignatureShareTable for this txid exists. All mint
    // outputs of a given tx are signed atomically in the same consensus
    // commit, so observing one implies all are present.
    let (shares, _dbtx) = mint
        .db
        .wait_table_check(&BlindedSignatureShareTable, |dbtx| {
            Some(collect_signature_shares(dbtx, req.txid)).filter(|s| !s.is_empty())
        })
        .await;

    Ok(SignatureSharesResponse { shares })
}

/// Callers establish membership through [`issuance_state`] first, so every
/// message here is expected to resolve and a miss is an error.
pub fn signature_shares_restore(
    mint: &Mint,
    req: SignatureSharesRestoreRequest,
) -> Result<SignatureSharesRestoreResponse, String> {
    let mut shares = Vec::new();

    let dbtx = mint.db.begin_read();

    for message in req.messages {
        let share = dbtx
            .get(&BlindedSignatureShareRestoreTable, &message)
            .ok_or_else(|| "No blinded signature share found".to_string())?;

        shares.push(share);
    }

    Ok(SignatureSharesRestoreResponse { shares })
}

pub fn issuance_state(
    mint: &Mint,
    req: IssuanceStateRequest,
) -> Result<IssuanceStateResponse, String> {
    let dbtx = mint.db.begin_read();

    let issued = req
        .messages
        .iter()
        .map(|message| dbtx.get(&BlindedNonceTable, message))
        .collect();

    Ok(IssuanceStateResponse { issued })
}

pub fn spend_state(mint: &Mint, req: SpendStateRequest) -> Result<SpendStateResponse, String> {
    let dbtx = mint.db.begin_read();

    let spent = req
        .nonces
        .iter()
        .map(|nonce| dbtx.get(&NoteNonceTable, nonce).is_some())
        .collect();

    Ok(SpendStateResponse { spent })
}

fn collect_signature_shares(dbtx: &ReadTx, txid: TransactionId) -> Vec<BlindedSignatureShare> {
    let bounds = OutPoint { txid, out_idx: 0 }..=OutPoint {
        txid,
        out_idx: u16::MAX,
    };

    dbtx.range(&BlindedSignatureShareTable, bounds, |r| {
        r.map(|(_, v)| v).collect()
    })
}
