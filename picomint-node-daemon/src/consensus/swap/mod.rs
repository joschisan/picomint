mod db;
mod rpc;

use anyhow::{Context, ensure};
use group::Curve;
use picomint_core::secp256k1::XOnlyPublicKey;
use picomint_core::swap::broker::BrokerPk;
use picomint_core::swap::config::{SwapConfig, SwapConfigConsensus, SwapConfigPrivate};
use picomint_core::swap::methods::SwapMethod;
use picomint_core::swap::{
    MINIMUM_RECEIVE_CONTRACT_AMOUNT, ReceiveContractSummary, SwapInput, SwapInputError, SwapOutput,
    SwapOutputError,
};
use picomint_core::{Amount, OutPoint};
use picomint_redb::{DbRead, WriteTx};
use tbs::{AggregatePublicKey, BlindedNonce, PublicKeyShare, SecretKeyShare, derive_pk_share};

use crate::config::NodeConfig;
use crate::config::dkg::DkgHandle;
use crate::config::poly::eval_poly_g2;
use crate::consensus::server::Server;
use crate::{handler, handler_async};

use self::db::{
    AttestationShareTable, BrokerTable, ReceiveContractIndexTable,
    ReceiveContractStreamNextIndexTable, ReceiveContractStreamTable, ReceiveContractTable,
    SendContractTable,
};

/// Run DKG for the swap module, producing a fresh `SwapConfig` for this
/// node: the attestation key, one G2 key like a note denomination's.
pub async fn dkg(nodes: &DkgHandle<'_>) -> anyhow::Result<SwapConfig> {
    let (polynomial, sk) = nodes.run_dkg_g2().await?;

    Ok(SwapConfig {
        consensus: SwapConfigConsensus {
            agg_pk: AggregatePublicKey(polynomial[0].to_affine()),
            pks: nodes
                .num_nodes()
                .node_ids()
                .map(|node| (node, PublicKeyShare(eval_poly_g2(&polynomial, &node))))
                .collect(),
            input_fee: Amount::from_sat(1),
            output_fee: Amount::from_sat(1),
        },
        private: SwapConfigPrivate {
            sk: SecretKeyShare(sk),
        },
    })
}

/// Verify our private attestation share matches the public share in the
/// consensus config.
pub fn validate_config(cfg: &NodeConfig) -> anyhow::Result<()> {
    ensure!(
        derive_pk_share(&cfg.private.swap.sk)
            == *cfg
                .consensus
                .swap
                .pks
                .get(&cfg.private.identity)
                .context("Public key set has no key for our identity")?,
        "Swap attestation secret key share does not match our public key share"
    );

    Ok(())
}

pub fn process_input(
    _server: &Server,
    dbtx: &WriteTx,
    input: &SwapInput,
) -> Result<(Amount, XOnlyPublicKey), SwapInputError> {
    match input {
        SwapInput::ClaimSend(outpoint, attestation) => {
            let contract = dbtx
                .remove(&SendContractTable, outpoint)
                .ok_or(SwapInputError::UnknownContract)?;

            if !contract.verify_attestation(attestation) {
                return Err(SwapInputError::InvalidAttestation);
            }

            Ok((contract.amount, contract.claim_pk))
        }
        SwapInput::ClaimReceive(outpoint) => {
            let contract = dbtx
                .remove(&ReceiveContractTable, outpoint)
                .ok_or(SwapInputError::UnknownContract)?;

            let index = dbtx
                .remove(&ReceiveContractIndexTable, outpoint)
                .expect("Receive contract index should exist");

            dbtx.remove(&ReceiveContractStreamTable, &index);

            Ok((contract.amount, contract.claim_pk))
        }
    }
}

pub fn process_output(
    server: &Server,
    dbtx: &WriteTx,
    output: &SwapOutput,
    outpoint: OutPoint,
) -> Result<Amount, SwapOutputError> {
    match output {
        SwapOutput::Send(contract) => {
            dbtx.insert(&SendContractTable, &outpoint, contract);

            Ok(contract.amount)
        }
        SwapOutput::Receive(contract) => {
            if contract.amount < MINIMUM_RECEIVE_CONTRACT_AMOUNT {
                return Err(SwapOutputError::AmountTooSmall);
            }

            dbtx.insert(&ReceiveContractTable, &outpoint, contract);

            dbtx.insert(
                &AttestationShareTable,
                &outpoint,
                &tbs::sign_nonce(
                    BlindedNonce(contract.id().message().0),
                    server.cfg.private.swap.sk,
                ),
            );

            let stream_index = dbtx
                .get(&ReceiveContractStreamNextIndexTable, &())
                .unwrap_or(0);

            dbtx.insert(
                &ReceiveContractStreamTable,
                &stream_index,
                &ReceiveContractSummary::new(outpoint, contract),
            );

            dbtx.insert(&ReceiveContractIndexTable, &outpoint, &stream_index);

            dbtx.insert(
                &ReceiveContractStreamNextIndexTable,
                &(),
                &(stream_index + 1),
            );

            Ok(contract.amount)
        }
    }
}

pub async fn handle_api(server: &Server, method: SwapMethod) -> Result<Vec<u8>, String> {
    match method {
        SwapMethod::AttestationShare(req) => handler_async!(attestation_share, server, req).await,
        SwapMethod::SendContract(req) => handler_async!(send_contract, server, req).await,
        SwapMethod::AwaitReceiveContracts(req) => {
            handler_async!(await_receive_contracts, server, req).await
        }
        SwapMethod::Brokers(req) => handler!(brokers, server, req).await,
    }
}

pub fn add_broker(server: &Server, pk: BrokerPk, name: String) -> bool {
    let dbtx = server.db.begin_write();
    let is_new_entry = dbtx.insert(&BrokerTable, &pk, &name).is_none();
    dbtx.commit();
    is_new_entry
}

pub fn remove_broker(server: &Server, pk: BrokerPk) -> bool {
    let dbtx = server.db.begin_write();
    let entry_existed = dbtx.remove(&BrokerTable, &pk).is_some();
    dbtx.commit();
    entry_existed
}

/// The named brokers this node has registered, for display and admin.
pub fn brokers(dbtx: &impl DbRead) -> Vec<(BrokerPk, String)> {
    dbtx.iter(&BrokerTable, |r| r.collect())
}
