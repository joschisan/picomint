//! Freestanding API handlers for [`super::Lightning`].

use std::time::Duration;

use picomint_core::ln::methods::{
    AwaitIncomingContractsRequest, AwaitIncomingContractsResponse, AwaitPreimageRequest,
    AwaitPreimageResponse, ConsensusBlockCountRequest, ConsensusBlockCountResponse,
    DecryptionKeyShareRequest, DecryptionKeyShareResponse, GatewaysRequest, GatewaysResponse,
    OutgoingContractExpiryRequest, OutgoingContractExpiryResponse,
};
use tokio::time::timeout;

use super::Lightning;
use super::db::{
    DecryptionKeyShareTable, GatewayTable, IncomingContractStreamIndexTable,
    IncomingContractStreamTable, OutgoingContractTable, PreimageTable,
};

pub fn consensus_block_count(
    ln: &Lightning,
    _: ConsensusBlockCountRequest,
) -> Result<ConsensusBlockCountResponse, String> {
    let dbtx = ln.db.begin_read();
    Ok(ConsensusBlockCountResponse {
        count: ln.consensus_block_count(&dbtx),
    })
}

pub async fn await_preimage(
    ln: &Lightning,
    req: AwaitPreimageRequest,
) -> Result<AwaitPreimageResponse, String> {
    loop {
        let wait = ln.db.wait_table_check(&PreimageTable, |dbtx| {
            dbtx.get(&PreimageTable, &req.outpoint)
        });

        if let Ok((preimage, _dbtx)) = timeout(Duration::from_secs(10), wait).await {
            return Ok(AwaitPreimageResponse {
                preimage: Some(preimage),
            });
        }

        let dbtx = ln.db.begin_read();

        if let Some(preimage) = dbtx.get(&PreimageTable, &req.outpoint) {
            return Ok(AwaitPreimageResponse {
                preimage: Some(preimage),
            });
        }

        if req.expiry <= ln.consensus_block_count(&dbtx) {
            return Ok(AwaitPreimageResponse { preimage: None });
        }
    }
}

pub fn decryption_key_share(
    ln: &Lightning,
    req: DecryptionKeyShareRequest,
) -> Result<DecryptionKeyShareResponse, String> {
    ln.db
        .begin_read()
        .get(&DecryptionKeyShareTable, &req.outpoint)
        .map(|share| DecryptionKeyShareResponse { share })
        .ok_or_else(|| "No decryption key share found".to_string())
}

pub fn outgoing_contract_expiry(
    ln: &Lightning,
    req: OutgoingContractExpiryRequest,
) -> Result<OutgoingContractExpiryResponse, String> {
    let dbtx = ln.db.begin_read();

    let Some(contract) = dbtx.get(&OutgoingContractTable, &req.outpoint) else {
        return Ok(OutgoingContractExpiryResponse { contract: None });
    };

    let expiry = contract
        .expiry
        .saturating_sub(ln.consensus_block_count(&dbtx));

    Ok(OutgoingContractExpiryResponse {
        contract: Some((contract.contract_id(), expiry)),
    })
}

pub async fn await_incoming_contracts(
    ln: &Lightning,
    req: AwaitIncomingContractsRequest,
) -> Result<AwaitIncomingContractsResponse, String> {
    if req.batch == 0 {
        return Err("Batch size must be greater than 0".to_string());
    }

    let (mut next_index, dbtx) = ln
        .db
        .wait_table_check(&IncomingContractStreamIndexTable, |dbtx| {
            dbtx.get(&IncomingContractStreamIndexTable, &())
                .filter(|i| *i > req.start)
        })
        .await;

    let entries = dbtx.range(&IncomingContractStreamTable, req.start..u64::MAX, |r| {
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

pub fn gateways(ln: &Lightning, _: GatewaysRequest) -> Result<GatewaysResponse, String> {
    Ok(GatewaysResponse {
        gateways: ln
            .db
            .begin_read()
            .iter(&GatewayTable, |r| r.map(|(pk, ())| pk).collect()),
    })
}
