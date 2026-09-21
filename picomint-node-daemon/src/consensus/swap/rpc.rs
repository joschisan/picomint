//! Freestanding API handlers for the swap module.

use picomint_core::swap::methods::{
    AttestationShareRequest, AttestationShareResponse, AwaitReceiveContractsRequest,
    AwaitReceiveContractsResponse, BrokersRequest, BrokersResponse, SendContractRequest,
    SendContractResponse,
};
use picomint_redb::DbRead;

use crate::consensus::server::Server;

use super::db::{
    AttestationShareTable, BrokerTable, ReceiveContractStreamNextIndexTable,
    ReceiveContractStreamTable, SendContractTable,
};

/// Waits for the share rather than reporting its absence, so a broker can
/// ask while its funding transaction is still in flight and have the share
/// the moment the contract is processed.
pub async fn attestation_share(
    server: &Server,
    req: AttestationShareRequest,
) -> Result<AttestationShareResponse, String> {
    let (share, _dbtx) = server
        .db
        .wait_table_check(&AttestationShareTable, |dbtx| {
            dbtx.get(&AttestationShareTable, &req.outpoint)
        })
        .await;

    Ok(AttestationShareResponse { share })
}

/// Waits for the contract rather than reporting its absence, so a broker
/// can be asked to swap while the funding transaction is still in flight.
pub async fn send_contract(
    server: &Server,
    req: SendContractRequest,
) -> Result<SendContractResponse, String> {
    let (contract, _dbtx) = server
        .db
        .wait_table_check(&SendContractTable, |dbtx| {
            dbtx.get(&SendContractTable, &req.outpoint)
        })
        .await;

    Ok(SendContractResponse { contract })
}

pub async fn await_receive_contracts(
    server: &Server,
    req: AwaitReceiveContractsRequest,
) -> Result<AwaitReceiveContractsResponse, String> {
    if req.batch == 0 {
        return Err("Batch size must be greater than 0".to_string());
    }

    let (mut next_index, dbtx) = server
        .db
        .wait_table_check(&ReceiveContractStreamNextIndexTable, |dbtx| {
            dbtx.get(&ReceiveContractStreamNextIndexTable, &())
                .filter(|i| *i > req.start)
        })
        .await;

    let entries = dbtx.range(&ReceiveContractStreamTable, req.start..u64::MAX, |r| {
        r.take(req.batch as usize).collect::<Vec<_>>()
    });

    let mut contracts = Vec::with_capacity(entries.len());

    for (key, entry) in entries {
        contracts.push(entry);
        next_index = key + 1;
    }

    Ok(AwaitReceiveContractsResponse {
        contracts,
        next_index,
    })
}

pub fn brokers(server: &Server, _: BrokersRequest) -> Result<BrokersResponse, String> {
    Ok(BrokersResponse {
        brokers: server
            .db
            .begin_read()
            .iter(&BrokerTable, |r| r.map(|(pk, _)| pk).collect()),
    })
}
