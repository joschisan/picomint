pub use picomint_core::lightning as common;

mod db;
mod rpc;

use anyhow::{Context, ensure};
use group::Curve;
use picomint_core::lightning::config::{
    LightningConfig, LightningConfigConsensus, LightningConfigPrivate,
};
use picomint_core::lightning::contracts::IncomingContractSummary;
use picomint_core::lightning::gateway::GatewayPk;
use picomint_core::lightning::methods::LightningMethod;
use picomint_core::lightning::{
    LightningInput, LightningInputError, LightningOutput, LightningOutputError, OutgoingWitness,
};
use picomint_core::secp256k1::XOnlyPublicKey;
use picomint_core::{Amount, OutPoint};
use picomint_redb::{DbRead, WriteTx};
use tpe::{PublicKeyShare, SecretKeyShare};

use crate::config::ServerConfig;
use crate::config::dkg::DkgHandle;
use crate::config::poly::eval_poly_g1;
use crate::consensus::db::consensus_block_count;
use crate::consensus::server::Server;
use crate::{handler, handler_async};

use self::db::{
    DecryptionKeyShareTable, GatewayTable, IncomingContractIndexTable,
    IncomingContractStreamIndexTable, IncomingContractStreamTable, IncomingContractTable,
    OutgoingContractTable, PreimageTable,
};

/// Run DKG for the lightning module, producing a fresh `LightningConfig` for
/// this peer.
pub async fn distributed_gen(peers: &DkgHandle<'_>) -> anyhow::Result<LightningConfig> {
    let (polynomial, sks) = peers.run_dkg_g1().await?;

    Ok(LightningConfig {
        consensus: LightningConfigConsensus {
            tpe_agg_pk: tpe::AggregatePublicKey(polynomial[0].to_affine()),
            tpe_pks: peers
                .num_peers()
                .peer_ids()
                .map(|peer| (peer, PublicKeyShare(eval_poly_g1(&polynomial, &peer))))
                .collect(),
            input_fee: Amount::from_sat(1),
            output_fee: Amount::from_sat(1),
        },
        private: LightningConfigPrivate {
            sk: SecretKeyShare(sks),
        },
    })
}

/// Verify our private tpe share matches the public share in the consensus
/// config.
pub fn validate_config(cfg: &ServerConfig) -> anyhow::Result<()> {
    ensure!(
        tpe::derive_pk_share(&cfg.private.lightning.sk)
            == *cfg
                .consensus
                .lightning
                .tpe_pks
                .get(&cfg.private.identity)
                .context("Public key set has no key for our identity")?,
        "Preimge encryption secret key share does not match our public key share"
    );

    Ok(())
}

pub fn process_input(
    server: &Server,
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
                    if contract.expiry <= consensus_block_count(server, dbtx) {
                        return Err(LightningInputError::Expired);
                    }

                    if !contract.verify_preimage(preimage) {
                        return Err(LightningInputError::InvalidPreimage);
                    }

                    dbtx.insert(&PreimageTable, outpoint, preimage);

                    contract.claim_pk
                }
                OutgoingWitness::Refund => {
                    if contract.expiry > consensus_block_count(server, dbtx) {
                        return Err(LightningInputError::NotExpired);
                    }

                    contract.refund_pk
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
        LightningInput::Incoming(outpoint, agg_decryption_key) => {
            let contract = dbtx
                .remove(&IncomingContractTable, outpoint)
                .ok_or(LightningInputError::UnknownContract)?;

            let index = dbtx
                .remove(&IncomingContractIndexTable, outpoint)
                .expect("Incoming contract index should exist");

            dbtx.remove(&IncomingContractStreamTable, &index);

            if !contract
                .offer
                .verify_agg_decryption_key(&server.cfg.consensus.lightning.tpe_agg_pk, agg_decryption_key)
            {
                return Err(LightningInputError::InvalidDecryptionKey);
            }

            let pub_key = match contract.offer.decrypt_preimage(agg_decryption_key) {
                Some(..) => contract.offer.commitment.claim_pk,
                None => contract.refund_pk,
            };

            let amount = contract
                .offer
                .commitment
                .amount
                .checked_sub(contract.offer.commitment.fee)
                .ok_or(LightningInputError::ArithmeticOverflow)?;

            Ok((amount, pub_key))
        }
    }
}

pub fn process_output(
    server: &Server,
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
            if !contract.offer.verify() {
                return Err(LightningOutputError::InvalidContract);
            }

            dbtx.insert(&IncomingContractTable, &outpoint, contract);

            let stream_index = dbtx
                .get(&IncomingContractStreamIndexTable, &())
                .unwrap_or(0);

            dbtx.insert(
                &IncomingContractStreamTable,
                &stream_index,
                &IncomingContractSummary::new(outpoint, &contract.offer),
            );

            dbtx.insert(&IncomingContractIndexTable, &outpoint, &stream_index);

            dbtx.insert(&IncomingContractStreamIndexTable, &(), &(stream_index + 1));

            let dk_share = contract
                .offer
                .create_decryption_key_share(&server.cfg.private.lightning.sk);

            dbtx.insert(&DecryptionKeyShareTable, &outpoint, &dk_share);

            contract
                .offer
                .commitment
                .amount
                .checked_sub(contract.offer.commitment.fee)
                .ok_or(LightningOutputError::ArithmeticOverflow)
        }
    }
}

/// Both incoming and outgoing contracts represent liabilities to the
/// mint since they are obligations to issue notes. The amount
/// the mint has actually locked per contract has to match the
/// arithmetic in [`process_input`] / [`process_output`]:
/// outgoing locks `amount + fee` (the gateway claims that on payout,
/// or the sender does on refund); incoming locks `amount - fee` (the
/// recipient claims that on success, with `fee` accruing to the
/// mint as implicit revenue).
pub fn audit(dbtx: &WriteTx) -> i64 {
    let outgoing: i64 = dbtx.iter(&OutgoingContractTable, |r| {
        r.map(|(_, contract)| -((contract.amount.msat + contract.fee.msat) as i64))
            .sum()
    });

    let incoming: i64 = dbtx.iter(&IncomingContractTable, |r| {
        r.map(|(_, contract)| {
            -((contract.offer.commitment.amount.msat - contract.offer.commitment.fee.msat) as i64)
        })
        .sum()
    });

    outgoing + incoming
}

pub async fn handle_api(server: &Server, method: LightningMethod) -> Result<Vec<u8>, String> {
    match method {
        LightningMethod::AwaitPreimage(req) => handler_async!(await_preimage, server, req).await,
        LightningMethod::DecryptionKeyShare(req) => handler!(decryption_key_share, server, req).await,
        LightningMethod::OutgoingContractExpiry(req) => {
            handler!(outgoing_contract_expiry, server, req).await
        }
        LightningMethod::AwaitIncomingContracts(req) => {
            handler_async!(await_incoming_contracts, server, req).await
        }
        LightningMethod::Gateways(req) => handler!(gateways, server, req).await,
        LightningMethod::TpeAggregatePk(req) => handler!(tpe_aggregate_pk, server, req).await,
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

/// The named gateways this guardian has registered, for display and admin.
pub fn gateways(dbtx: &impl DbRead) -> Vec<(GatewayPk, String)> {
    dbtx.iter(&GatewayTable, |r| r.collect())
}
