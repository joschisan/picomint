//! Freestanding API handlers for the lightning module.

use picomint_core::lightning::methods::{
    AwaitIncomingContractsRequest, AwaitIncomingContractsResponse, AwaitOutgoingContractRequest,
    AwaitOutgoingContractResponse, AwaitPreimageRequest, AwaitPreimageResponse, GatewaysRequest,
    GatewaysResponse,
};

use picomint_redb::DbRead;

use crate::consensus::server::Server;

use super::db::{
    GatewayTable, IncomingContractStreamNextIndexTable, IncomingContractStreamTable,
    OutgoingContractTable, PreimageTable,
};

/// Waits for the preimage rather than reporting its absence: an outgoing
/// contract settles only through the gateway, by claim or by forfeit, so
/// there is no block height at which waiting stops being worthwhile.
pub async fn await_preimage(
    server: &Server,
    req: AwaitPreimageRequest,
) -> Result<AwaitPreimageResponse, String> {
    let (preimage, _dbtx) = server
        .db
        .wait_table_check(&PreimageTable, |dbtx| {
            dbtx.get(&PreimageTable, &req.outpoint)
        })
        .await;

    Ok(AwaitPreimageResponse { preimage })
}

/// Waits for the contract rather than reporting its absence, so a
/// gateway can be asked to pay while the funding transaction is still
/// in flight.
pub async fn await_outgoing_contract(
    server: &Server,
    req: AwaitOutgoingContractRequest,
) -> Result<AwaitOutgoingContractResponse, String> {
    let (contract, _dbtx) = server
        .db
        .wait_table_check(&OutgoingContractTable, |dbtx| {
            dbtx.get(&OutgoingContractTable, &req.outpoint)
        })
        .await;

    Ok(AwaitOutgoingContractResponse {
        contract: contract.contract_id(),
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
        .wait_table_check(&IncomingContractStreamNextIndexTable, |dbtx| {
            dbtx.get(&IncomingContractStreamNextIndexTable, &())
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
