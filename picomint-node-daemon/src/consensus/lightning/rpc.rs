//! Freestanding API handlers for the lightning module.

use std::time::Duration;

use picomint_core::lightning::methods::{
    AwaitIncomingContractsRequest, AwaitIncomingContractsResponse, AwaitPreimageRequest,
    AwaitPreimageResponse, DecryptionKeyShareRequest, DecryptionKeyShareResponse, GatewaysRequest,
    GatewaysResponse, OutgoingContractExpiryRequest, OutgoingContractExpiryResponse,
    TpeAggregatePkRequest, TpeAggregatePkResponse,
};
use tokio::time::timeout;

use picomint_redb::DbRead;

use crate::consensus::db::consensus_block_count;
use crate::consensus::server::Server;

use super::db::{
    DecryptionKeyShareTable, GatewayTable, IncomingContractStreamIndexTable,
    IncomingContractStreamTable, OutgoingContractTable, PreimageTable,
};

pub async fn await_preimage(
    server: &Server,
    req: AwaitPreimageRequest,
) -> Result<AwaitPreimageResponse, String> {
    loop {
        let wait = server.db.wait_table_check(&PreimageTable, |dbtx| {
            dbtx.get(&PreimageTable, &req.outpoint)
        });

        if let Ok((preimage, _dbtx)) = timeout(Duration::from_secs(10), wait).await {
            return Ok(AwaitPreimageResponse {
                preimage: Some(preimage),
            });
        }

        let dbtx = server.db.begin_read();

        if let Some(preimage) = dbtx.get(&PreimageTable, &req.outpoint) {
            return Ok(AwaitPreimageResponse {
                preimage: Some(preimage),
            });
        }

        if req.expiry <= consensus_block_count(server, &dbtx) {
            return Ok(AwaitPreimageResponse { preimage: None });
        }
    }
}

pub fn decryption_key_share(
    server: &Server,
    req: DecryptionKeyShareRequest,
) -> Result<DecryptionKeyShareResponse, String> {
    server
        .db
        .begin_read()
        .get(&DecryptionKeyShareTable, &req.outpoint)
        .map(|share| DecryptionKeyShareResponse { share })
        .ok_or_else(|| "No decryption key share found".to_string())
}

pub fn outgoing_contract_expiry(
    server: &Server,
    req: OutgoingContractExpiryRequest,
) -> Result<OutgoingContractExpiryResponse, String> {
    let dbtx = server.db.begin_read();

    let Some(contract) = dbtx.get(&OutgoingContractTable, &req.outpoint) else {
        return Ok(OutgoingContractExpiryResponse { contract: None });
    };

    let expiry = contract
        .expiry
        .saturating_sub(consensus_block_count(server, &dbtx));

    Ok(OutgoingContractExpiryResponse {
        contract: Some((contract.contract_id(), expiry)),
    })
}

pub async fn await_incoming_contracts(
    server: &Server,
    req: AwaitIncomingContractsRequest,
) -> Result<AwaitIncomingContractsResponse, String> {
    if req.batch == 0 {
        return Err("Batch size must be greater than 0".to_string());
    }

    let (mut next_index, dbtx) = server
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

pub fn gateways(server: &Server, _: GatewaysRequest) -> Result<GatewaysResponse, String> {
    Ok(GatewaysResponse {
        gateways: server
            .db
            .begin_read()
            .iter(&GatewayTable, |r| r.map(|(pk, _)| pk).collect()),
    })
}

/// The mint's tpe aggregate key. Ungated for the same reason as
/// `mint_info`: it is public to every client, and a caller holding a
/// hash of it out of band can check what it gets.
pub fn tpe_aggregate_pk(
    server: &Server,
    _: TpeAggregatePkRequest,
) -> Result<TpeAggregatePkResponse, String> {
    Ok(TpeAggregatePkResponse {
        tpe_agg_pk: server.cfg.consensus.lightning.tpe_agg_pk,
    })
}
