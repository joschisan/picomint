pub use picomint_core::ln as common;

mod db;
mod rpc;

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, ensure};
use group::Curve;
use picomint_bitcoin_rpc::BitcoinRpcMonitor;
use picomint_core::ln::config::{
    LightningConfig, LightningConfigConsensus, LightningConfigPrivate,
};
use picomint_core::ln::gateway_api::GatewayPk;
use picomint_core::ln::methods::LnMethod;
use picomint_core::ln::{
    LightningConsensusItem, LightningInput, LightningInputError, LightningOutput,
    LightningOutputError, OutgoingWitness,
};
use picomint_core::module::{InputMeta, TxItemAmounts};
use picomint_core::{Amount, NumPeersExt, OutPoint, PeerId};
use picomint_redb::{Database, ReadTx, WriteTx};
use tpe::{PublicKeyShare, SecretKeyShare};
use tracing::trace;

use crate::config::dkg::DkgHandle;
use crate::config::poly::eval_poly_g1;
use crate::{handler, handler_async};

use self::db::{
    BlockCountVoteTable, DecryptionKeyShareTable, GatewayTable, IncomingContractIndexTable,
    IncomingContractStreamIndexTable, IncomingContractStreamTable, IncomingContractTable,
    OutgoingContractTable, PreimageTable, UnixTimeVoteTable,
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
            input_fee: Amount::from_sats(1),
            output_fee: Amount::from_sats(1),
        },
        private: LightningConfigPrivate {
            sk: SecretKeyShare(sks),
        },
    })
}

/// Verify our private tpe share matches the public share in the consensus
/// config.
pub fn validate_config(identity: &PeerId, cfg: &LightningConfig) -> anyhow::Result<()> {
    ensure!(
        tpe::derive_pk_share(&cfg.private.sk)
            == *cfg
                .consensus
                .tpe_pks
                .get(identity)
                .context("Public key set has no key for our identity")?,
        "Preimge encryption secret key share does not match our public key share"
    );

    Ok(())
}

pub struct Lightning {
    cfg: LightningConfig,
    db: Database,
    server_bitcoin_rpc_monitor: BitcoinRpcMonitor,
}

impl Lightning {
    #[must_use]
    pub fn new(
        cfg: LightningConfig,
        db: Database,
        server_bitcoin_rpc_monitor: BitcoinRpcMonitor,
    ) -> Self {
        Self {
            cfg,
            db,
            server_bitcoin_rpc_monitor,
        }
    }
}

impl Lightning {
    pub async fn consensus_proposal(&self, _dbtx: &ReadTx) -> Vec<LightningConsensusItem> {
        // We reduce the time granularity to deduplicate votes more often and not save
        // one consensus item every second.
        let unix_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("System time before Unix epoch")
            .as_secs();
        let mut items = vec![LightningConsensusItem::UnixTimeVote(60 * (unix_secs / 60))];

        if let Ok(block_count) = self.get_block_count() {
            trace!(?block_count, "Proposing block count");
            items.push(LightningConsensusItem::BlockCountVote(block_count));
        }

        items
    }

    pub async fn process_consensus_item(
        &self,
        dbtx: &WriteTx,
        peer: PeerId,
        consensus_item: LightningConsensusItem,
    ) -> anyhow::Result<()> {
        trace!(?consensus_item, "Processing consensus item proposal");

        match consensus_item {
            LightningConsensusItem::BlockCountVote(vote) => {
                let current_vote = dbtx.insert(&BlockCountVoteTable, &peer, &vote).unwrap_or(0);

                ensure!(current_vote < vote, "Block count vote is redundant");

                Ok(())
            }
            LightningConsensusItem::UnixTimeVote(vote) => {
                let current_vote = dbtx.insert(&UnixTimeVoteTable, &peer, &vote).unwrap_or(0);

                ensure!(current_vote < vote, "Unix time vote is redundant");

                Ok(())
            }
        }
    }

    pub async fn process_input(
        &self,
        dbtx: &WriteTx,
        input: &LightningInput,
    ) -> Result<InputMeta, LightningInputError> {
        let (pub_key, amount) = match input {
            LightningInput::Outgoing(outpoint, outgoing_witness) => {
                let contract = dbtx
                    .remove(&OutgoingContractTable, outpoint)
                    .ok_or(LightningInputError::UnknownContract)?;

                let pub_key = match outgoing_witness {
                    OutgoingWitness::Claim(preimage) => {
                        if contract.expiration <= self.consensus_block_count(dbtx) {
                            return Err(LightningInputError::Expired);
                        }

                        if !contract.verify_preimage(preimage) {
                            return Err(LightningInputError::InvalidPreimage);
                        }

                        dbtx.insert(&PreimageTable, outpoint, preimage);

                        contract.claim_pk
                    }
                    OutgoingWitness::Refund => {
                        if contract.expiration > self.consensus_block_count(dbtx) {
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

                (pub_key, amount)
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
                    .verify_agg_decryption_key(&self.cfg.consensus.tpe_agg_pk, agg_decryption_key)
                {
                    return Err(LightningInputError::InvalidDecryptionKey);
                }

                let pub_key = match contract.decrypt_preimage(agg_decryption_key) {
                    Some(..) => contract.commitment.claim_pk,
                    None => contract.commitment.refund_pk,
                };

                let amount = contract
                    .commitment
                    .amount
                    .checked_sub(contract.commitment.fee)
                    .ok_or(LightningInputError::ArithmeticOverflow)?;

                (pub_key, amount)
            }
        };

        Ok(InputMeta {
            amount: TxItemAmounts {
                amount,
                fee: self.cfg.consensus.input_fee,
            },
            pub_key,
        })
    }

    pub async fn process_output(
        &self,
        dbtx: &WriteTx,
        output: &LightningOutput,
        outpoint: OutPoint,
    ) -> Result<TxItemAmounts, LightningOutputError> {
        let amount = match output {
            LightningOutput::Outgoing(contract) => {
                let amount = contract
                    .amount
                    .checked_add(contract.fee)
                    .ok_or(LightningOutputError::ArithmeticOverflow)?;

                dbtx.insert(&OutgoingContractTable, &outpoint, contract);

                amount
            }
            LightningOutput::Incoming(contract) => {
                if !contract.verify() {
                    return Err(LightningOutputError::InvalidContract);
                }

                if contract.commitment.expiration <= self.consensus_unix_time(dbtx) {
                    return Err(LightningOutputError::ContractExpired);
                }

                dbtx.insert(&IncomingContractTable, &outpoint, contract);

                let stream_index = dbtx
                    .get(&IncomingContractStreamIndexTable, &())
                    .unwrap_or(0);

                dbtx.insert(
                    &IncomingContractStreamTable,
                    &stream_index,
                    &(outpoint, contract.clone()),
                );

                dbtx.insert(&IncomingContractIndexTable, &outpoint, &stream_index);

                dbtx.insert(&IncomingContractStreamIndexTable, &(), &(stream_index + 1));

                let dk_share = contract.create_decryption_key_share(&self.cfg.private.sk);

                dbtx.insert(&DecryptionKeyShareTable, &outpoint, &dk_share);

                contract
                    .commitment
                    .amount
                    .checked_sub(contract.commitment.fee)
                    .ok_or(LightningOutputError::ArithmeticOverflow)?
            }
        };

        Ok(TxItemAmounts {
            amount,
            fee: self.cfg.consensus.output_fee,
        })
    }

    /// Both incoming and outgoing contracts represent liabilities to the
    /// federation since they are obligations to issue notes. The amount
    /// the federation has actually locked per contract has to match the
    /// arithmetic in [`Self::process_input`] / [`Self::process_output`]:
    /// outgoing locks `amount + fee` (the gateway claims that on payout,
    /// or the sender does on refund); incoming locks `amount - fee` (the
    /// recipient claims that on success, with `fee` accruing to the
    /// federation as implicit revenue).
    pub async fn audit(&self, dbtx: &WriteTx) -> i64 {
        let outgoing: i64 = dbtx.iter(&OutgoingContractTable, |r| {
            r.map(|(_, contract)| -((contract.amount.msats + contract.fee.msats) as i64))
                .sum()
        });

        let incoming: i64 = dbtx.iter(&IncomingContractTable, |r| {
            r.map(|(_, contract)| {
                -((contract.commitment.amount.msats - contract.commitment.fee.msats) as i64)
            })
            .sum()
        });

        outgoing + incoming
    }

    pub async fn handle_api(&self, method: LnMethod) -> Result<Vec<u8>, String> {
        match method {
            LnMethod::ConsensusBlockCount(req) => handler!(consensus_block_count, self, req).await,
            LnMethod::AwaitPreimage(req) => handler_async!(await_preimage, self, req).await,
            LnMethod::DecryptionKeyShare(req) => handler!(decryption_key_share, self, req).await,
            LnMethod::OutgoingContractExpiration(req) => {
                handler!(outgoing_contract_expiration, self, req).await
            }
            LnMethod::AwaitIncomingContracts(req) => {
                handler_async!(await_incoming_contracts, self, req).await
            }
            LnMethod::Gateways(req) => handler!(gateways, self, req).await,
        }
    }
}

impl Lightning {
    fn get_block_count(&self) -> anyhow::Result<u64> {
        self.server_bitcoin_rpc_monitor
            .status()
            .map(|status| status.block_count)
            .context("Block count not available yet")
    }

    pub(crate) fn consensus_block_count(&self, dbtx: &impl picomint_redb::DbRead) -> u64 {
        let num_peers = self.cfg.consensus.tpe_pks.to_num_peers();

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

    pub(crate) fn consensus_unix_time(&self, dbtx: &impl picomint_redb::DbRead) -> u64 {
        let num_peers = self.cfg.consensus.tpe_pks.to_num_peers();

        let mut times = dbtx.iter(&UnixTimeVoteTable, |r| {
            r.map(|(_, v)| v).collect::<Vec<u64>>()
        });

        times.sort_unstable();

        times.reverse();

        assert!(times.last() <= times.first());

        times.get(num_peers.threshold() - 1).copied().unwrap_or(0)
    }

    #[must_use]
    pub fn consensus_block_count_ui(&self) -> u64 {
        self.consensus_block_count(&self.db.begin_read())
    }

    #[must_use]
    pub fn consensus_unix_time_ui(&self) -> u64 {
        self.consensus_unix_time(&self.db.begin_read())
    }

    pub async fn add_gateway_ui(&self, gateway_pk: GatewayPk) -> bool {
        let dbtx = self.db.begin_write();
        let is_new_entry = dbtx.insert(&GatewayTable, &gateway_pk, &()).is_none();
        dbtx.commit();
        is_new_entry
    }

    pub async fn remove_gateway_ui(&self, gateway_pk: GatewayPk) -> bool {
        let dbtx = self.db.begin_write();
        let entry_existed = dbtx.remove(&GatewayTable, &gateway_pk).is_some();
        dbtx.commit();
        entry_existed
    }

    #[must_use]
    pub fn gateways_ui(&self) -> Vec<GatewayPk> {
        self.db
            .begin_read()
            .iter(&GatewayTable, |r| r.map(|(pk, ())| pk).collect())
    }
}
