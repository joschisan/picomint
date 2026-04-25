//! Freestanding API handlers for [`super::Lightning`].

use std::time::Duration;

use picomint_core::ln::methods::{
    AwaitIncomingContractsRequest, AwaitIncomingContractsResponse, AwaitPreimageRequest,
    AwaitPreimageResponse, ConsensusBlockCountRequest, ConsensusBlockCountResponse,
    DecryptionKeyShareRequest, DecryptionKeyShareResponse, GatewaysRequest, GatewaysResponse,
    OutgoingContractExpirationRequest, OutgoingContractExpirationResponse,
};
use picomint_core::module::ApiError;
use tokio::time::timeout;

use super::Lightning;
use super::db::{
    DECRYPTION_KEY_SHARE, GATEWAY, INCOMING_CONTRACT_STREAM, INCOMING_CONTRACT_STREAM_INDEX,
    OUTGOING_CONTRACT, PREIMAGE,
};

pub fn consensus_block_count(
    ln: &Lightning,
    _: ConsensusBlockCountRequest,
) -> Result<ConsensusBlockCountResponse, ApiError> {
    let tx = ln.db.begin_read();
    Ok(ConsensusBlockCountResponse {
        count: ln.consensus_block_count(&tx),
    })
}

pub async fn await_preimage(
    ln: &Lightning,
    req: AwaitPreimageRequest,
) -> Result<AwaitPreimageResponse, ApiError> {
    loop {
        let wait = ln
            .db
            .wait_table_check(&PREIMAGE, |tx| tx.get(&PREIMAGE, &req.outpoint));

        if let Ok((preimage, _tx)) = timeout(Duration::from_secs(10), wait).await {
            return Ok(AwaitPreimageResponse {
                preimage: Some(preimage),
            });
        }

        let tx = ln.db.begin_read();

        if let Some(preimage) = tx.get(&PREIMAGE, &req.outpoint) {
            return Ok(AwaitPreimageResponse {
                preimage: Some(preimage),
            });
        }

        if req.expiration <= ln.consensus_block_count(&tx) {
            return Ok(AwaitPreimageResponse { preimage: None });
        }
    }
}

pub fn decryption_key_share(
    ln: &Lightning,
    req: DecryptionKeyShareRequest,
) -> Result<DecryptionKeyShareResponse, ApiError> {
    ln.db
        .begin_read()
        .get(&DECRYPTION_KEY_SHARE, &req.outpoint)
        .map(|share| DecryptionKeyShareResponse { share })
        .ok_or_else(|| ApiError::bad_request("No decryption key share found".to_string()))
}

pub fn outgoing_contract_expiration(
    ln: &Lightning,
    req: OutgoingContractExpirationRequest,
) -> Result<OutgoingContractExpirationResponse, ApiError> {
    let tx = ln.db.begin_read();

    let Some(contract) = tx.get(&OUTGOING_CONTRACT, &req.outpoint) else {
        return Ok(OutgoingContractExpirationResponse { contract: None });
    };

    let expiration = contract
        .expiration
        .saturating_sub(ln.consensus_block_count(&tx));

    Ok(OutgoingContractExpirationResponse {
        contract: Some((contract.contract_id(), expiration)),
    })
}

pub async fn await_incoming_contracts(
    ln: &Lightning,
    req: AwaitIncomingContractsRequest,
) -> Result<AwaitIncomingContractsResponse, ApiError> {
    if req.batch == 0 {
        return Err(ApiError::bad_request(
            "Batch size must be greater than 0".to_string(),
        ));
    }

    let (mut next_index, tx) = ln
        .db
        .wait_table_check(&INCOMING_CONTRACT_STREAM_INDEX, |tx| {
            tx.get(&INCOMING_CONTRACT_STREAM_INDEX, &())
                .filter(|i| *i > req.start)
        })
        .await;

    let entries = tx.range(&INCOMING_CONTRACT_STREAM, req.start..u64::MAX, |r| {
        r.take(req.batch as usize).collect::<Vec<_>>()
    });

    let mut contracts = Vec::with_capacity(entries.len());

    for (key, entry) in entries {
        contracts.push(entry);
        next_index = key + 1;
    }

    Ok(AwaitIncomingContractsResponse {
        contracts,
        next_index,
    })
}

pub fn gateways(ln: &Lightning, _: GatewaysRequest) -> Result<GatewaysResponse, ApiError> {
    Ok(GatewaysResponse {
        gateways: ln.db.begin_read().iter(&GATEWAY, |r| {
            r.filter_map(|(bytes, ())| iroh::PublicKey::from_bytes(&bytes).ok())
                .collect()
        }),
    })
}
