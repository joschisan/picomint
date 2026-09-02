//! Freestanding API handlers for [`crate::consensus::api::ConsensusApi`].

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use picomint_core::methods::{
    ConfigRequest, ConfigResponse, ExpiryStatusRequest, ExpiryStatusResponse,
    FederationInfoRequest, FederationInfoResponse, LivenessRequest, LivenessResponse,
    SubmitTxRequest, SubmitTxResponse,
};
use picomint_core::tx::{ConsensusItem, Transaction, TxError};
use picomint_redb::DbRead;
use tracing::{info, warn};

use crate::consensus::api::ConsensusApi;
use crate::consensus::db::{
    AcceptedItemTable, AcceptedTxTable, InviteMetaTable, InviteUserCountTable,
    SignedSessionOutcomeTable,
};
use crate::consensus::rpc;
use crate::{handler, handler_async};
use picomint_core::methods::CoreMethod;

pub async fn handle_api(api: &ConsensusApi, method: CoreMethod) -> Result<Vec<u8>, String> {
    match method {
        CoreMethod::SubmitTx(req) => handler_async!(submit_tx, api, req).await,
        CoreMethod::Config(req) => handler!(config, api, req).await,
        CoreMethod::Liveness(req) => handler!(liveness, api, req).await,
        CoreMethod::ExpiryStatus(req) => handler!(expiry_status, api, req).await,
        CoreMethod::FederationInfo(req) => handler!(federation_info, api, req).await,
    }
}

/// Submit a transaction and long-poll until it is either accepted by
/// consensus or becomes invalid. On acceptance, logs the wall-clock from
/// submission to confirmation, so the server side of client-observed
/// latency can be profiled straight from the guardian's `info` logs.
pub async fn submit_tx(
    api: &ConsensusApi,
    req: SubmitTxRequest,
) -> Result<SubmitTxResponse, String> {
    Ok(SubmitTxResponse {
        outcome: await_tx_outcome(api, req.tx).await,
    })
}

async fn await_tx_outcome(api: &ConsensusApi, tx: Transaction) -> Result<(), TxError> {
    // Consensus checks these too, but that is after the transaction has
    // been proposed, and a transaction we propose travels in a bft unit
    // our peers have to be able to receive. The counts are what hold a
    // submission to a size that fits one; the rest is refusing to carry a
    // transaction consensus is certain to throw out.
    if tx.inputs.is_empty() {
        return Err(TxError::EmptyInputs);
    }

    if tx.outputs.is_empty() {
        return Err(TxError::EmptyOutputs);
    }

    if tx.inputs.len() > Transaction::MAX_INPUTS {
        return Err(TxError::TooManyInputs);
    }

    if tx.outputs.len() > Transaction::MAX_OUTPUTS {
        return Err(TxError::TooManyOutputs);
    }

    if tx.signatures.len() != tx.inputs.len() {
        return Err(TxError::InvalidWitnessLength);
    }

    let start = Instant::now();

    // Subscribe before submitting so a rejection cannot land in the gap.
    let mut rejections = api.server.tx_reject_tx.subscribe();

    let notify_item = api.server.db.notify_for_table(&AcceptedItemTable);
    let notify_session = api.server.db.notify_for_table(&SignedSessionOutcomeTable);

    let mut notified_item = Box::pin(notify_item.notified());
    let mut notified_session = Box::pin(notify_session.notified());

    if api
        .server
        .db
        .begin_read()
        .get(&AcceptedTxTable, &tx.compute_txid())
        .is_some()
    {
        return Ok(());
    }

    if api
        .submission_tx
        .send(ConsensusItem::Tx(tx.clone()))
        .await
        .is_err()
    {
        warn!("Unable to submit the tx into consensus");
    }

    loop {
        tokio::select! {
            _ = &mut notified_item => {
                if api.server.db.begin_read().get(&AcceptedTxTable, &tx.compute_txid()).is_some() {
                    info!(
                        txid = %tx.compute_txid(),
                        elapsed_ms = start.elapsed().as_millis() as u64,
                        "Submission RPC confirmed tx",
                    );

                    return Ok(());
                }

                notified_item = Box::pin(notify_item.notified());
            }
            rejection = rejections.recv() => {
                let (rejected, error) =
                    rejection.expect("The tx rejection broadcast failed");

                if rejected == tx.compute_txid() {
                    return Err(error);
                }
            }
            _ = &mut notified_session => {
                if api
                    .submission_tx
                    .send(ConsensusItem::Tx(tx.clone()))
                    .await
                    .is_err()
                {
                    warn!("Unable to submit the tx into consensus");
                }

                notified_session = Box::pin(notify_session.notified());
            }
        }
    }
}

/// Check the expiration date and user limit of the invite code with this
/// invite id and count the download towards its user limit before serving
/// the config. Errors (surfaced to the client) for unknown, expired, or
/// exhausted invite codes.
pub fn config(api: &ConsensusApi, req: ConfigRequest) -> Result<ConfigResponse, String> {
    let dbtx = api.server.db.begin_write();

    let meta = dbtx
        .get(&InviteMetaTable, &req.invite_id)
        .ok_or_else(|| "Unknown invite id".to_string())?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the unix epoch")
        .as_secs();

    if meta.expires_at <= now {
        return Err("Invite code is expired".to_string());
    }

    let users = dbtx.get(&InviteUserCountTable, &req.invite_id).unwrap_or(0);

    if users >= meta.user_limit {
        return Err("Invite code has reached its user limit".to_string());
    }

    dbtx.insert(&InviteUserCountTable, &req.invite_id, &(users + 1));

    dbtx.commit();

    Ok(ConfigResponse {
        config: api.server.cfg.consensus.clone(),
    })
}

pub fn liveness(_: &ConsensusApi, _: LivenessRequest) -> Result<LivenessResponse, String> {
    Ok(LivenessResponse)
}

pub fn expiry_status(
    api: &ConsensusApi,
    _: ExpiryStatusRequest,
) -> Result<ExpiryStatusResponse, String> {
    Ok(ExpiryStatusResponse {
        status: api.expiry_status(),
    })
}

/// Ungated, unlike [`config`]: the federation id and peer set are already held
/// by any joined client, and a caller that got them out of band can pin them
/// against a hash. Serving them grants nothing an invite would otherwise gate.
pub fn federation_info(
    api: &ConsensusApi,
    _: FederationInfoRequest,
) -> Result<FederationInfoResponse, String> {
    Ok(FederationInfoResponse::new(&api.server.cfg.consensus))
}
