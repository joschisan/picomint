pub use picomint_core::lightning as common;

mod db;
mod rpc;

use picomint_core::lightning::config::LightningConfigConsensus;
use picomint_core::lightning::gateway::GatewayPk;
use picomint_core::lightning::methods::LightningMethod;
use picomint_core::lightning::{
    LightningInput, LightningInputError, LightningOutput, LightningOutputError, OutgoingWitness,
};
use picomint_core::secp256k1::XOnlyPublicKey;
use picomint_core::{Amount, OutPoint};
use picomint_redb::{DbRead, WriteTx};

use crate::consensus::server::Server;
use crate::{handler, handler_async};

use self::db::{
    GatewayTable, IncomingContractIndexTable, IncomingContractStreamNextIndexTable,
    IncomingContractStreamTable, IncomingContractTable, IncomingPaymentTable,
    OutgoingContractTable, PreimageTable,
};

/// The lightning module's consensus config. The module holds no keys: an
/// incoming contract is spent by the recipient's own claim key and an
/// outgoing one by the gateway's, so there is nothing for the nodes to
/// share.
pub fn config() -> LightningConfigConsensus {
    LightningConfigConsensus {
        input_fee: Amount::from_sat(1),
        output_fee: Amount::from_sat(1),
    }
}

pub fn process_input(
    dbtx: &WriteTx,
    input: &LightningInput,
) -> Result<(Amount, XOnlyPublicKey), LightningInputError> {
    match input {
        LightningInput::Outgoing(outpoint, outgoing_witness) => {
            let contract = dbtx
                .remove(&OutgoingContractTable, outpoint)
                .ok_or(LightningInputError::UnknownContract)?;

            let pub_key = match outgoing_witness {
                OutgoingWitness::Claim(preimage) => {
                    if !contract.verify_preimage(preimage) {
                        return Err(LightningInputError::InvalidPreimage);
                    }

                    dbtx.insert(&PreimageTable, outpoint, preimage);

                    contract.claim_pk
                }
                OutgoingWitness::Cancel(forfeit_signature) => {
                    if !contract.verify_forfeit_signature(forfeit_signature) {
                        return Err(LightningInputError::InvalidForfeitSignature);
                    }

                    contract.refund_pk
                }
            };

            let amount = contract
                .amount
                .checked_add(contract.fee)
                .ok_or(LightningInputError::ArithmeticOverflow)?;

            Ok((amount, pub_key))
        }
        LightningInput::Incoming(outpoint) => {
            let contract = dbtx
                .remove(&IncomingContractTable, outpoint)
                .ok_or(LightningInputError::UnknownContract)?;

            let index = dbtx
                .remove(&IncomingContractIndexTable, outpoint)
                .expect("Incoming contract index should exist");

            dbtx.remove(&IncomingContractStreamTable, &index);

            let amount = contract
                .claim_amount()
                .ok_or(LightningInputError::ArithmeticOverflow)?;

            Ok((amount, contract.claim_pk))
        }
    }
}

pub fn process_output(
    dbtx: &WriteTx,
    output: &LightningOutput,
    outpoint: OutPoint,
) -> Result<Amount, LightningOutputError> {
    match output {
        LightningOutput::Outgoing(contract) => {
            let amount = contract
                .amount
                .checked_add(contract.fee)
                .ok_or(LightningOutputError::ArithmeticOverflow)?;

            dbtx.insert(&OutgoingContractTable, &outpoint, contract);

            Ok(amount)
        }
        LightningOutput::Incoming(contract) => {
            let amount = contract
                .claim_amount()
                .ok_or(LightningOutputError::ArithmeticOverflow)?;

            dbtx.insert(&IncomingContractTable, &outpoint, contract);

            dbtx.insert(
                &IncomingPaymentTable,
                &contract.payment_hash(),
                &contract.preimage(),
            );

            let stream_index = dbtx
                .get(&IncomingContractStreamNextIndexTable, &())
                .unwrap_or(0);

            dbtx.insert(
                &IncomingContractStreamTable,
                &stream_index,
                &(outpoint, contract.clone()),
            );

            dbtx.insert(&IncomingContractIndexTable, &outpoint, &stream_index);

            dbtx.insert(
                &IncomingContractStreamNextIndexTable,
                &(),
                &(stream_index + 1),
            );

            Ok(amount)
        }
    }
}

pub async fn handle_api(server: &Server, method: LightningMethod) -> Result<Vec<u8>, String> {
    match method {
        LightningMethod::AwaitPreimage(req) => handler_async!(await_preimage, server, req).await,
        LightningMethod::AwaitOutgoingContract(req) => {
            handler_async!(await_outgoing_contract, server, req).await
        }
        LightningMethod::AwaitIncomingContracts(req) => {
            handler_async!(await_incoming_contracts, server, req).await
        }
        LightningMethod::IncomingPayment(req) => handler!(incoming_payment, server, req).await,
        LightningMethod::AwaitIncomingPayment(req) => {
            handler_async!(await_incoming_payment, server, req).await
        }
        LightningMethod::Gateways(req) => handler!(gateways, server, req).await,
    }
}

pub fn add_gateway(server: &Server, pk: GatewayPk, name: String) -> bool {
    let dbtx = server.db.begin_write();
    let is_new_entry = dbtx.insert(&GatewayTable, &pk, &name).is_none();
    dbtx.commit();
    is_new_entry
}

pub fn remove_gateway(server: &Server, pk: GatewayPk) -> bool {
    let dbtx = server.db.begin_write();
    let entry_existed = dbtx.remove(&GatewayTable, &pk).is_some();
    dbtx.commit();
    entry_existed
}

/// The named gateways this node has registered, for display and admin.
pub fn gateways(dbtx: &impl DbRead) -> Vec<(GatewayPk, String)> {
    dbtx.iter(&GatewayTable, |r| r.collect())
}
