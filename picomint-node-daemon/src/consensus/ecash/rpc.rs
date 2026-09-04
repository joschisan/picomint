//! Freestanding API handlers for the ecash module.

use picomint_core::OutPoint;
use picomint_core::TransactionId;
use picomint_core::ecash::methods::{
    IssuanceStateRequest, IssuanceStateResponse, SignaturesRequest, SignaturesResponse,
    SignaturesRestoreRequest, SignaturesRestoreResponse, SpendStateRequest, SpendStateResponse,
};
use picomint_redb::{DbRead, ReadTx};
use tbs::BlindedSignatureShare;

use crate::consensus::server::Server;

use super::db::{
    BlindedNonceTable, BlindedSignatureShareRestoreTable, BlindedSignatureShareTable,
    NoteNonceTable,
};

pub async fn signatures(
    server: &Server,
    req: SignaturesRequest,
) -> Result<SignaturesResponse, String> {
    // Wait until any BlindedSignatureShareTable for this txid exists. All mint
    // outputs of a given tx are signed atomically in the same consensus
    // commit, so observing one implies all are present.
    let (shares, _dbtx) = server
        .db
        .wait_table_check(&BlindedSignatureShareTable, |dbtx| {
            Some(collect_signatures(dbtx, req.txid)).filter(|s| !s.is_empty())
        })
        .await;

    Ok(SignaturesResponse { shares })
}

/// Callers establish membership through [`issuance_state`] first, so every
/// message here is expected to resolve and a miss is an error.
pub fn signatures_restore(
    server: &Server,
    req: SignaturesRestoreRequest,
) -> Result<SignaturesRestoreResponse, String> {
    let mut shares = Vec::new();

    let dbtx = server.db.begin_read();

    for message in req.messages {
        let share = dbtx
            .get(&BlindedSignatureShareRestoreTable, &message)
            .ok_or_else(|| "No blinded signature share found".to_string())?;

        shares.push(share);
    }

    Ok(SignaturesRestoreResponse { shares })
}

pub fn issuance_state(
    server: &Server,
    req: IssuanceStateRequest,
) -> Result<IssuanceStateResponse, String> {
    let dbtx = server.db.begin_read();

    let issued = req
        .messages
        .iter()
        .map(|message| dbtx.get(&BlindedNonceTable, message))
        .collect();

    Ok(IssuanceStateResponse { issued })
}

pub fn spend_state(server: &Server, req: SpendStateRequest) -> Result<SpendStateResponse, String> {
    let dbtx = server.db.begin_read();

    let spent = req
        .nonces
        .iter()
        .map(|nonce| dbtx.get(&NoteNonceTable, nonce).is_some())
        .collect();

    Ok(SpendStateResponse { spent })
}

fn collect_signatures(dbtx: &ReadTx, txid: TransactionId) -> Vec<BlindedSignatureShare> {
    let bounds = OutPoint { txid, out_idx: 0 }..=OutPoint {
        txid,
        out_idx: u16::MAX,
    };

    dbtx.range(&BlindedSignatureShareTable, bounds, |r| {
        r.map(|(_, v)| v).collect()
    })
}
