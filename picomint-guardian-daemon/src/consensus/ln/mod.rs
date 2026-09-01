pub use picomint_core::ln as common;

mod db;
mod rpc;

use anyhow::{Context, ensure};
use group::Curve;
use picomint_core::ln::config::{
    LightningConfig, LightningConfigConsensus, LightningConfigPrivate,
};
use picomint_core::ln::contracts::IncomingContractSummary;
use picomint_core::ln::gateway::GatewayPk;
use picomint_core::ln::methods::LnMethod;
use picomint_core::ln::{
    LightningConsensusItem, LightningInput, LightningInputError, LightningOutput,
    LightningOutputError, OutgoingWitness,
};
use picomint_core::secp256k1::XOnlyPublicKey;
use picomint_core::{Amount, NumPeersExt, OutPoint, PeerId};
use picomint_redb::{DbRead, ReadTx, WriteTx};
use tpe::{PublicKeyShare, SecretKeyShare};
use tracing::trace;

use crate::config::ServerConfig;
use crate::config::dkg::DkgHandle;
use crate::config::poly::eval_poly_g1;
use crate::consensus::server::Server;
use crate::{handler, handler_async};

use self::db::{
    BlockCountVoteTable, DecryptionKeyShareTable, GatewayTable, IncomingContractIndexTable,
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
        tpe::derive_pk_share(&cfg.private.ln.sk)
            == *cfg
                .consensus
                .ln
                .tpe_pks
                .get(&cfg.private.identity)
                .context("Public key set has no key for our identity")?,
        "Preimge encryption secret key share does not match our public key share"
    );

    Ok(())
}

pub fn consensus_proposal(server: &Server, dbtx: &ReadTx) -> Vec<LightningConsensusItem> {
    let mut items = Vec::new();

    if let Ok(block_count) = get_block_count(server)
        && block_count
            > dbtx
                .get(&BlockCountVoteTable, &server.cfg.private.identity)
                .unwrap_or(0)
    {
        items.push(LightningConsensusItem::BlockCount(block_count));
    }

    items
}

pub fn process_consensus_item(
    _server: &Server,
    dbtx: &WriteTx,
    peer: PeerId,
    consensus_item: LightningConsensusItem,
) -> anyhow::Result<()> {
    trace!(?consensus_item, "Processing consensus item proposal");

    match consensus_item {
        LightningConsensusItem::BlockCount(vote) => {
            let current_vote = dbtx.insert(&BlockCountVoteTable, &peer, &vote).unwrap_or(0);

            ensure!(current_vote < vote, "Block count vote is redundant");

            Ok(())
        }
    }
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
                .verify_agg_decryption_key(&server.cfg.consensus.ln.tpe_agg_pk, agg_decryption_key)
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
                .create_decryption_key_share(&server.cfg.private.ln.sk);

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
/// federation since they are obligations to issue notes. The amount
/// the federation has actually locked per contract has to match the
/// arithmetic in [`process_input`] / [`process_output`]:
/// outgoing locks `amount + fee` (the gateway claims that on payout,
/// or the sender does on refund); incoming locks `amount - fee` (the
/// recipient claims that on success, with `fee` accruing to the
/// federation as implicit revenue).
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

pub async fn handle_api(server: &Server, method: LnMethod) -> Result<Vec<u8>, String> {
    match method {
        LnMethod::ConsensusBlockCount(req) => handler!(consensus_block_count, server, req).await,
        LnMethod::AwaitPreimage(req) => handler_async!(await_preimage, server, req).await,
        LnMethod::DecryptionKeyShare(req) => handler!(decryption_key_share, server, req).await,
        LnMethod::OutgoingContractExpiry(req) => {
            handler!(outgoing_contract_expiry, server, req).await
        }
        LnMethod::AwaitIncomingContracts(req) => {
            handler_async!(await_incoming_contracts, server, req).await
        }
        LnMethod::Gateways(req) => handler!(gateways, server, req).await,
        LnMethod::TpeAggregatePk(req) => handler!(tpe_aggregate_pk, server, req).await,
    }
}

fn get_block_count(server: &Server) -> anyhow::Result<u64> {
    server
        .btc_rpc
        .status()
        .map(|status| status.block_count)
        .context("Block count not available yet")
}

pub(crate) fn consensus_block_count(server: &Server, dbtx: &impl DbRead) -> u64 {
    let num_peers = server.cfg.consensus.ln.tpe_pks.to_num_peers();

    let mut counts = dbtx.iter(&BlockCountVoteTable, |r| {
        r.map(|(_, v)| v).collect::<Vec<u64>>()
    });

    counts.sort_unstable();

    counts.reverse();

    assert!(counts.last() <= counts.first());

    // The block count we select guarantees that any threshold of correct peers can
    // increase the consensus block count and any consensus block count has been
    // confirmed by a threshold of peers.

    counts.get(num_peers.threshold() - 1).copied().unwrap_or(0)
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
