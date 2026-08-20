pub mod db;
mod rpc;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use self::db::{
    BlockCountVoteTable, FederationWalletTable, FeeRateVoteTable, NonceEntry, NonceLogTable,
    Output, OutputTable, Signatures, SignaturesTable, SpentOutputTable, TxInfoIndexTable,
    TxInfoTable, TxidKey, UnconfirmedTxTable, UnsignedTxTable,
};
use anyhow::{Context, anyhow, ensure};
use bitcoin::absolute::LockTime;
use bitcoin::hashes::{Hash, sha256};
use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
use bitcoin::transaction::Version;
use bitcoin::{Amount, Network, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness};
use common::config::WalletConfigConsensus;
use common::{OutputInfo, WalletConsensusItem, WalletInput, WalletOutput};
use picomint_bitcoin_rpc::BitcoinRpcMonitor;
use picomint_core::backoff::{Retryable, networking_backoff};
use picomint_core::module::{InputMeta, TxItemAmounts};
use picomint_core::wallet as common;
use picomint_core::{NumPeersExt, OutPoint, PeerId};
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::{Database, ReadTx, WriteTx};
use tokio::time::sleep;

use crate::config::dkg::DkgHandle;
use crate::config::dkg_secp::eval_poly;
use crate::handler;
use picomint_core::secret::Secret;
use picomint_core::wallet::config::{WalletConfig, WalletConfigPrivate};
use picomint_core::wallet::methods::WalletMethod;
use picomint_core::wallet::{
    FederationWallet, TxInfo, WalletInputError, WalletOutputError, is_potential_receive,
    tweak_public_key, tweaked_script_pubkey,
};
use secp256k1::Scalar;
use serde::{Deserialize, Serialize};
use tracing::info;
use tss::{
    AggregatePublicKey, PublicKeyShare, PublicNonce, SecretKeyShare, SecretNonce, SignatureShare,
    aggregate_signature_shares, derive_nonce, derive_pk_share, derive_public_nonce, sign_share,
    verify_signature_share,
};

/// Number of confirmations required for a transaction to be considered as
/// final by the federation. The block that mines the transaction does
/// not count towards the number of confirmations.
pub const CONFIRMATION_FINALITY_DELAY: u64 = 6;

/// Maximum number of blocks the consensus block count can advance in a single
/// consensus item to limit the work done in one `process_consensus_item` step.
const MAX_BLOCK_COUNT_INCREMENT: u64 = 50;

/// Minimum fee rate vote of 1 sat/vB to ensure we never propose a fee rate
/// below what Bitcoin Core will relay.
const MIN_FEERATE_VOTE_SATS_PER_KVB: u64 = 1000;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Encodable, Decodable)]
pub struct FederationTx {
    pub tx: Transaction,
    pub spent_tx_outs: Vec<SpentTxOut>,
    pub vbytes: u64,
    pub fee: Amount,
}

picomint_redb::consensus_value!(FederationTx);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encodable, Decodable)]
pub struct SpentTxOut {
    pub value: Amount,
    pub tweak: sha256::Hash,
}

fn pending_txs_unordered(dbtx: &impl picomint_redb::DbRead) -> Vec<FederationTx> {
    let unsigned: Option<FederationTx> = dbtx.get(&UnsignedTxTable, &());

    let unconfirmed: Vec<FederationTx> =
        dbtx.iter(&UnconfirmedTxTable, |r| r.map(|(_, v)| v).collect());

    unsigned.into_iter().chain(unconfirmed).collect()
}

/// Run DKG for the wallet module, producing a fresh `WalletConfig` for this
/// peer.
pub async fn distributed_gen(peers: &DkgHandle<'_>) -> anyhow::Result<WalletConfig> {
    let (polynomial, sks) = peers.run_dkg_secp().await?;

    let pks = peers
        .num_peers()
        .peer_ids()
        .map(|peer| Ok((peer, PublicKeyShare(eval_poly(&polynomial, &peer)?))))
        .collect::<anyhow::Result<BTreeMap<PeerId, PublicKeyShare>>>()?;

    Ok(WalletConfig {
        private: WalletConfigPrivate {
            sks: SecretKeyShare(sks),
        },
        consensus: WalletConfigConsensus::new(AggregatePublicKey(polynomial[0]), pks),
    })
}

/// Verify our wallet secret key share matches the corresponding public key
/// share in the consensus config.
pub fn validate_config(identity: &PeerId, cfg: &WalletConfig) -> anyhow::Result<()> {
    ensure!(
        cfg.consensus
            .pks
            .get(identity)
            .context("Public key share set has no key for our identity")?
            == &derive_pk_share(&cfg.private.sks),
        "Wallet secret key share does not match our public key share"
    );

    Ok(())
}

impl Wallet {
    pub async fn consensus_proposal(&self, dbtx: &ReadTx) -> Vec<WalletConsensusItem> {
        let mut items: Vec<WalletConsensusItem> = dbtx
            .get(&UnsignedTxTable, &())
            .and_then(|unsigned_tx| self.signing_session_proposal(dbtx, &unsigned_tx))
            .into_iter()
            .collect();

        let feerate_vote = self.btc_rpc.status().map(|status| {
            status
                .fee_rate
                .sat_per_kvb
                .max(MIN_FEERATE_VOTE_SATS_PER_KVB)
        });

        // `None` retracts our vote while the bitcoin backend is down.
        if dbtx.get(&FeeRateVoteTable, &self.identity) != Some(feerate_vote) {
            items.push(WalletConsensusItem::Feerate(feerate_vote));
        }

        if let Some(status) = self.btc_rpc.status() {
            let block_count_vote = status
                .block_count
                .saturating_sub(CONFIRMATION_FINALITY_DELAY);

            let consensus_block_count = self.consensus_block_count(dbtx);

            let block_count_vote = match consensus_block_count {
                0 => block_count_vote,
                _ => block_count_vote.min(consensus_block_count + MAX_BLOCK_COUNT_INCREMENT),
            };

            if block_count_vote > dbtx.get(&BlockCountVoteTable, &self.identity).unwrap_or(0) {
                items.push(WalletConsensusItem::BlockCount(block_count_vote));
            }
        }

        items
    }

    /// Determines the next item to propose for an unsigned transaction: our
    /// signature shares plus a fresh replacement nonce entry if we are a
    /// member of a signing session we have not signed yet, or our initial
    /// nonce entry if we have never entered the log. Items are re-proposed
    /// until they are accepted; the handlers reject duplicates.
    fn signing_session_proposal(
        &self,
        dbtx: &ReadTx,
        unsigned_tx: &FederationTx,
    ) -> Option<WalletConsensusItem> {
        let txid = unsigned_tx.tx.compute_txid();

        let inputs = unsigned_tx.spent_tx_outs.len();

        let latest = dbtx.iter(&NonceLogTable, |r| {
            r.rev()
                .find(|entry| entry.1.0 == self.identity)
                .map(|entry| entry.0 as usize)
        });

        let Some(latest) = latest else {
            let nonces = self.derive_secret_nonces(txid, 0, inputs);

            let public_nonces = nonces.iter().map(derive_public_nonce).collect();

            return Some(WalletConsensusItem::Nonces(txid, public_nonces));
        };

        let session = latest / self.threshold();

        // Our latest entry's session is always unsigned by us, since our
        // shares are stored in the same atomic step that appends our next
        // entry. If its chunk is still incomplete we idle; once it completes
        // we re-propose our deterministic shares until they are accepted.
        let chunk: Vec<NonceEntry> = dbtx.iter(&NonceLogTable, |r| {
            r.skip(session * self.threshold())
                .take(self.threshold())
                .map(|entry| entry.1)
                .collect()
        });

        if chunk.len() < self.threshold() {
            return None;
        }

        let sighashes = self.sighashes(unsigned_tx);

        let generation = dbtx.iter(&NonceLogTable, |r| {
            r.filter(|entry| entry.1.0 == self.identity).count() as u64 - 1
        });

        let nonces = self.derive_secret_nonces(txid, generation, inputs);

        let shares = self.sign_tx(unsigned_tx, &sighashes, nonces, &chunk);

        let fresh_nonces = self.derive_secret_nonces(txid, generation + 1, inputs);

        let public_nonces = fresh_nonces.iter().map(derive_public_nonce).collect();

        Some(WalletConsensusItem::Signatures(txid, shares, public_nonces))
    }

    /// Derives the secret nonces for our nonce entry of the given generation,
    /// where generation n is our n-th entry in the transaction's log.
    /// Deterministic derivation is safe here because the log's total order
    /// fixes each entry's signing session before any share is computed, the
    /// message is fixed by the txid and a txid is never signed as a fresh
    /// transaction twice - so every nonce meets exactly one signing context,
    /// even across crash recovery.
    fn derive_secret_nonces(&self, txid: Txid, generation: u64, inputs: usize) -> Vec<SecretNonce> {
        let secret = Secret::new_root(&self.cfg.private.sks)
            .child(&txid)
            .child(&generation);

        (0..inputs as u64)
            .map(|input| derive_nonce(&secret.child(&input).to_byte_array()))
            .collect()
    }

    fn threshold(&self) -> usize {
        self.cfg.consensus.pks.to_num_peers().threshold()
    }

    pub async fn process_consensus_item(
        &self,
        dbtx: &WriteTx,
        peer: PeerId,
        consensus_item: WalletConsensusItem,
    ) -> anyhow::Result<()> {
        match consensus_item {
            WalletConsensusItem::BlockCount(block_count_vote) => {
                self.process_block_count(dbtx, block_count_vote, peer).await
            }
            WalletConsensusItem::Feerate(feerate) => {
                if Some(feerate) == dbtx.insert(&FeeRateVoteTable, &peer, &feerate) {
                    return Err(anyhow!("Fee rate vote is redundant"));
                }

                Ok(())
            }
            WalletConsensusItem::Nonces(txid, nonces) => {
                self.process_nonces(dbtx, txid, nonces, peer)
            }
            WalletConsensusItem::Signatures(txid, shares, nonces) => {
                self.process_signatures(dbtx, txid, shares, nonces, peer)
                    .await
            }
        }
    }

    pub async fn process_input(
        &self,
        dbtx: &WriteTx,
        input: &WalletInput,
    ) -> Result<InputMeta, WalletInputError> {
        if dbtx
            .insert(&SpentOutputTable, &input.output_index, &())
            .is_some()
        {
            return Err(WalletInputError::OutputAlreadySpent);
        }

        let Output(tracked_outpoint, tracked_output) = dbtx
            .get(&OutputTable, &input.output_index)
            .ok_or(WalletInputError::UnknownOutputIndex)?;

        let tweaked_script = self.script_pubkey(&input.tweak.consensus_hash());

        if tracked_output.script_pubkey != tweaked_script {
            return Err(WalletInputError::WrongTweak);
        }

        let consensus_receive_fee = self
            .receive_fee(dbtx)
            .ok_or(WalletInputError::NoConsensusFeerateAvailable)?;

        // We allow for a higher fee such that a guardian could construct a CPFP
        // transaction. This is the last line of defense should the federations
        // transactions ever get stuck due to a critical failure of the feerate
        // estimation.
        if input.fee < consensus_receive_fee {
            return Err(WalletInputError::InsufficientTotalFee);
        }

        let output_value = tracked_output
            .value
            .checked_sub(input.fee)
            .ok_or(WalletInputError::ArithmeticOverflow)?;

        if let Some(wallet) = dbtx.remove(&FederationWalletTable, &()) {
            // Assuming the first receive into the federation is made through a
            // standard transaction, its output value is over the dust limit.
            // By induction so is this change value.
            let change_value = wallet
                .value
                .checked_add(output_value)
                .ok_or(WalletInputError::ArithmeticOverflow)?;

            let tx = Transaction {
                version: Version(2),
                lock_time: LockTime::ZERO,
                input: vec![
                    TxIn {
                        previous_output: wallet.outpoint,
                        script_sig: Default::default(),
                        sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                        witness: bitcoin::Witness::new(),
                    },
                    TxIn {
                        previous_output: tracked_outpoint,
                        script_sig: Default::default(),
                        sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                        witness: bitcoin::Witness::new(),
                    },
                ],
                output: vec![TxOut {
                    value: change_value,
                    script_pubkey: self.script_pubkey(&wallet.consensus_hash()),
                }],
            };

            dbtx.insert(
                &FederationWalletTable,
                &(),
                &FederationWallet {
                    value: change_value,
                    outpoint: bitcoin::OutPoint {
                        txid: tx.compute_txid(),
                        vout: 0,
                    },
                    tweak: wallet.consensus_hash(),
                },
            );

            let tx_index = Self::total_txs(dbtx);

            let created = self.consensus_block_count(dbtx);

            dbtx.insert(
                &TxInfoTable,
                &tx_index,
                &TxInfo {
                    index: tx_index,
                    txid: tx.compute_txid(),
                    input: wallet.value,
                    output: change_value,
                    vbytes: self.cfg.consensus.receive_tx_vbytes,
                    fee: input.fee,
                    created,
                },
            );

            let unsigned_tx = FederationTx {
                tx: tx.clone(),
                spent_tx_outs: vec![
                    SpentTxOut {
                        value: wallet.value,
                        tweak: wallet.tweak,
                    },
                    SpentTxOut {
                        value: tracked_output.value,
                        tweak: input.tweak.consensus_hash(),
                    },
                ],
                vbytes: self.cfg.consensus.receive_tx_vbytes,
                fee: input.fee,
            };

            if dbtx.insert(&UnsignedTxTable, &(), &unsigned_tx).is_some() {
                return Err(WalletInputError::PendingTransaction);
            }
        } else {
            dbtx.insert(
                &FederationWalletTable,
                &(),
                &FederationWallet {
                    value: tracked_output.value,
                    outpoint: tracked_outpoint,
                    tweak: input.tweak.consensus_hash(),
                },
            );
        }

        let amount = output_value
            .to_sat()
            .checked_mul(1000)
            .map(picomint_core::Amount::from_msat)
            .ok_or(WalletInputError::ArithmeticOverflow)?;

        Ok(InputMeta {
            amount: TxItemAmounts {
                amount,
                fee: self.cfg.consensus.input_fee,
            },
            pub_key: input.tweak,
        })
    }

    pub async fn process_output(
        &self,
        dbtx: &WriteTx,
        output: &WalletOutput,
        outpoint: OutPoint,
    ) -> Result<TxItemAmounts, WalletOutputError> {
        if output.value < self.cfg.consensus.dust_limit {
            return Err(WalletOutputError::UnderDustLimit);
        }

        let wallet = dbtx
            .remove(&FederationWalletTable, &())
            .ok_or(WalletOutputError::NoFederationUTXO)?;

        let consensus_send_fee = self
            .send_fee(dbtx)
            .ok_or(WalletOutputError::NoConsensusFeerateAvailable)?;

        // We allow for a higher fee such that a guardian could construct a CPFP
        // transaction. This is the last line of defense should the federations
        // transactions ever get stuck due to a critical failure of the feerate
        // estimation.
        if output.fee < consensus_send_fee {
            return Err(WalletOutputError::InsufficientTotalFee);
        }

        let output_value = output
            .value
            .checked_add(output.fee)
            .ok_or(WalletOutputError::ArithmeticOverflow)?;

        let change_value = wallet
            .value
            .checked_sub(output_value)
            .ok_or(WalletOutputError::ArithmeticOverflow)?;

        if change_value < self.cfg.consensus.dust_limit {
            return Err(WalletOutputError::ChangeUnderDustLimit);
        }

        let script_pubkey = output.destination.script_pubkey();

        let tx = Transaction {
            version: Version(2),
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: wallet.outpoint,
                script_sig: Default::default(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: bitcoin::Witness::new(),
            }],
            output: vec![
                TxOut {
                    value: change_value,
                    script_pubkey: self.script_pubkey(&wallet.consensus_hash()),
                },
                TxOut {
                    value: output.value,
                    script_pubkey,
                },
            ],
        };

        dbtx.insert(
            &FederationWalletTable,
            &(),
            &FederationWallet {
                value: change_value,
                outpoint: bitcoin::OutPoint {
                    txid: tx.compute_txid(),
                    vout: 0,
                },
                tweak: wallet.consensus_hash(),
            },
        );

        let tx_index = Self::total_txs(dbtx);

        let created = self.consensus_block_count(dbtx);

        dbtx.insert(
            &TxInfoTable,
            &tx_index,
            &TxInfo {
                index: tx_index,
                txid: tx.compute_txid(),
                input: wallet.value,
                output: change_value,
                vbytes: self.cfg.consensus.send_tx_vbytes,
                fee: output.fee,
                created,
            },
        );

        dbtx.insert(&TxInfoIndexTable, &outpoint, &tx_index);

        let unsigned_tx = FederationTx {
            tx: tx.clone(),
            spent_tx_outs: vec![SpentTxOut {
                value: wallet.value,
                tweak: wallet.tweak,
            }],
            vbytes: self.cfg.consensus.send_tx_vbytes,
            fee: output.fee,
        };

        if dbtx.insert(&UnsignedTxTable, &(), &unsigned_tx).is_some() {
            return Err(WalletOutputError::PendingTransaction);
        }

        let amount = output_value
            .to_sat()
            .checked_mul(1000)
            .map(picomint_core::Amount::from_msat)
            .ok_or(WalletOutputError::ArithmeticOverflow)?;

        Ok(TxItemAmounts {
            amount,
            fee: self.cfg.consensus.output_fee,
        })
    }

    pub async fn audit(&self, dbtx: &WriteTx) -> i64 {
        dbtx.get(&FederationWalletTable, &())
            .map_or(0, |wallet| 1000 * wallet.value.to_sat() as i64)
    }

    pub async fn handle_api(&self, method: WalletMethod) -> Result<Vec<u8>, String> {
        match method {
            WalletMethod::ConsensusBlockCount(req) => {
                handler!(consensus_block_count, self, req).await
            }
            WalletMethod::ConsensusFeerate(req) => handler!(consensus_feerate, self, req).await,
            WalletMethod::FederationWallet(req) => handler!(federation_wallet, self, req).await,
            WalletMethod::SendFee(req) => handler!(send_fee, self, req).await,
            WalletMethod::ReceiveFee(req) => handler!(receive_fee, self, req).await,
            WalletMethod::TxId(req) => handler!(tx_id, self, req).await,
            WalletMethod::OutputInfoSlice(req) => handler!(output_info_slice, self, req).await,
            WalletMethod::PendingTxChain(req) => handler!(pending_tx_chain, self, req).await,
            WalletMethod::TxChain(req) => handler!(tx_chain, self, req).await,
        }
    }
}

pub struct Wallet {
    identity: PeerId,
    cfg: WalletConfig,
    db: Database,
    btc_rpc: BitcoinRpcMonitor,
    network: Network,
}

impl Wallet {
    pub fn new(
        identity: PeerId,
        cfg: WalletConfig,
        db: Database,
        btc_rpc: BitcoinRpcMonitor,
        network: Network,
    ) -> Wallet {
        Self::spawn_broadcast_unconfirmed_txs_task(btc_rpc.clone(), db.clone(), network);

        Wallet {
            identity,
            cfg,
            db,
            btc_rpc,
            network,
        }
    }

    fn spawn_broadcast_unconfirmed_txs_task(
        btc_rpc: BitcoinRpcMonitor,
        db: Database,
        network: Network,
    ) {
        tokio::spawn(async move {
            loop {
                let unconfirmed_txs: Vec<FederationTx> = db
                    .begin_read()
                    .iter(&UnconfirmedTxTable, |r| r.map(|(_, v)| v).collect());

                for unconfirmed_tx in unconfirmed_txs {
                    btc_rpc.submit_tx(unconfirmed_tx.tx).await;
                }

                if network == Network::Regtest {
                    sleep(Duration::from_secs(1)).await;
                } else {
                    sleep(Duration::from_secs(60)).await;
                }
            }
        });
    }

    async fn process_block_count(
        &self,
        dbtx: &WriteTx,
        block_count_vote: u64,
        peer: PeerId,
    ) -> anyhow::Result<()> {
        let old_consensus_block_count = self.consensus_block_count(dbtx);

        let current_vote = dbtx
            .insert(&BlockCountVoteTable, &peer, &block_count_vote)
            .unwrap_or(0);

        ensure!(
            current_vote < block_count_vote,
            "Block count vote is redundant"
        );

        let new_consensus_block_count = self.consensus_block_count(dbtx);

        assert!(old_consensus_block_count <= new_consensus_block_count);

        if new_consensus_block_count != old_consensus_block_count {
            info!(
                %peer,
                block_count_vote,
                old_consensus_block_count,
                new_consensus_block_count,
                "consensus_block_count advanced"
            );
        }

        // We do not sync blocks that predate the federation itself.
        if old_consensus_block_count == 0 {
            return Ok(());
        }

        // Our bitcoin backend needs to be synced for the following calls to the
        // get_block rpc to be safe for consensus.
        self.await_local_sync_to_block_count(
            new_consensus_block_count + CONFIRMATION_FINALITY_DELAY,
        )
        .await;

        for height in old_consensus_block_count..new_consensus_block_count {
            let block_hash = (|| self.btc_rpc.get_block_hash(height))
                .retry(networking_backoff())
                .await
                .expect("networking_backoff retries forever");

            let block = (|| self.btc_rpc.get_block(&block_hash))
                .retry(networking_backoff())
                .await
                .expect("networking_backoff retries forever");

            assert_eq!(block.block_hash(), block_hash, "Block hash mismatch");

            let pks_hash = self.cfg.consensus.agg_pk.consensus_hash();

            for tx in block.txdata {
                dbtx.remove(&UnconfirmedTxTable, &TxidKey(tx.compute_txid()));

                // We maintain an append-only log of transaction outputs that pass
                // the probabilistic receive filter created since the federation was
                // established. This is downloaded by clients to detect pegins and
                // claim them by index.

                for (vout, tx_out) in tx.output.iter().enumerate() {
                    if is_potential_receive(&pks_hash, &tx_out.script_pubkey) {
                        let outpoint = bitcoin::OutPoint {
                            txid: tx.compute_txid(),
                            vout: u32::try_from(vout)
                                .expect("Bitcoin transaction has more than u32::MAX outputs"),
                        };

                        let index =
                            dbtx.iter(&OutputTable, |r| r.next_back().map_or(0, |(k, _)| k + 1));

                        dbtx.insert(&OutputTable, &index, &Output(outpoint, tx_out.clone()));
                    }
                }
            }
        }

        Ok(())
    }

    fn process_nonces(
        &self,
        dbtx: &WriteTx,
        txid: Txid,
        nonces: Vec<PublicNonce>,
        peer: PeerId,
    ) -> anyhow::Result<()> {
        let unsigned = dbtx
            .get(&UnsignedTxTable, &())
            .context("No unsigned transaction exists")?;

        ensure!(
            unsigned.tx.compute_txid() == txid,
            "Txid does not match the unsigned transaction"
        );

        ensure!(
            nonces.len() == unsigned.spent_tx_outs.len(),
            "Incorrect number of nonces"
        );

        ensure!(
            dbtx.iter(&NonceLogTable, |r| r.all(|entry| entry.1.0 != peer)),
            "Nonce entry is redundant"
        );

        let next_index = dbtx.iter(&NonceLogTable, |r| {
            r.next_back().map_or(0, |entry| entry.0 + 1)
        });

        dbtx.insert(&NonceLogTable, &next_index, &NonceEntry(peer, nonces));

        Ok(())
    }

    async fn process_signatures(
        &self,
        dbtx: &WriteTx,
        txid: Txid,
        shares: Vec<SignatureShare>,
        fresh_nonces: Vec<PublicNonce>,
        peer: PeerId,
    ) -> anyhow::Result<()> {
        let mut unsigned = dbtx
            .get(&UnsignedTxTable, &())
            .context("No unsigned transaction exists")?;

        ensure!(
            unsigned.tx.compute_txid() == txid,
            "Txid does not match the unsigned transaction"
        );

        ensure!(
            shares.len() == unsigned.spent_tx_outs.len(),
            "Incorrect number of signature shares"
        );

        ensure!(
            fresh_nonces.len() == unsigned.spent_tx_outs.len(),
            "Incorrect number of replacement nonces"
        );

        // The session of the peer's latest entry is the only one it might
        // not have signed yet, since appending an entry requires signing the
        // session of the previous one.
        let latest = dbtx
            .iter(&NonceLogTable, |r| {
                r.rev()
                    .find(|entry| entry.1.0 == peer)
                    .map(|entry| entry.0 as usize)
            })
            .context("Peer has no nonce entry")?;

        let session = latest / self.threshold();

        let chunk_range =
            (session * self.threshold()) as u64..((session + 1) * self.threshold()) as u64;

        let chunk: Vec<NonceEntry> = dbtx.range(&NonceLogTable, chunk_range.clone(), |r| {
            r.map(|entry| entry.1).collect()
        });

        ensure!(
            chunk.len() == self.threshold(),
            "The signing session of the peer's latest entry is still forming"
        );

        let sighashes = self.sighashes(&unsigned);

        for (index, ((utxo, msg), share)) in unsigned
            .spent_tx_outs
            .iter()
            .zip(&sighashes)
            .zip(&shares)
            .enumerate()
        {
            ensure!(
                verify_signature_share(
                    *msg,
                    peer.to_u64(),
                    &self.tweaked_pks(&peer, &utxo.tweak),
                    share,
                    &nonce_column(&chunk, index),
                    &self.tweaked_agg_pk(&utxo.tweak),
                ),
                "Invalid signature share"
            );
        }

        ensure!(
            dbtx.insert(&SignaturesTable, &(latest as u64), &Signatures(shares))
                .is_none(),
            "Already received signature shares for this entry"
        );

        let next_index = dbtx.iter(&NonceLogTable, |r| {
            r.next_back().map_or(0, |entry| entry.0 + 1)
        });

        dbtx.insert(&NonceLogTable, &next_index, &NonceEntry(peer, fresh_nonces));

        let responses: Vec<Signatures> = dbtx.range(&SignaturesTable, chunk_range, |r| {
            r.map(|(_, shares)| shares).collect()
        });

        if responses.len() == self.threshold() {
            self.finalize_tx(&mut unsigned, &sighashes, &chunk, &responses);

            dbtx.remove(&UnsignedTxTable, &());

            dbtx.delete_table(&NonceLogTable);

            dbtx.delete_table(&SignaturesTable);

            dbtx.insert(&UnconfirmedTxTable, &TxidKey(txid), &unsigned);

            self.btc_rpc.submit_tx(unsigned.tx).await;
        }

        Ok(())
    }

    async fn await_local_sync_to_block_count(&self, block_count: u64) {
        loop {
            if self
                .btc_rpc
                .status()
                .is_some_and(|status| status.block_count >= block_count)
            {
                break;
            }

            info!("Waiting for local bitcoin backend to sync to block count {block_count}");

            if self.network == Network::Regtest {
                sleep(Duration::from_secs(1)).await;
            } else {
                sleep(Duration::from_secs(60)).await;
            }
        }
    }

    pub fn consensus_block_count(&self, dbtx: &impl picomint_redb::DbRead) -> u64 {
        let num_peers = self.cfg.consensus.pks.to_num_peers();

        let mut counts: Vec<u64> = dbtx.iter(&BlockCountVoteTable, |r| r.map(|(_, v)| v).collect());

        assert!(counts.len() <= num_peers.total());

        counts.sort_unstable();

        counts.reverse();

        assert!(counts.last() <= counts.first());

        // The block count we select guarantees that any threshold of correct peers can
        // increase the consensus block count and any consensus block count has been
        // confirmed by a threshold of peers.

        counts.get(num_peers.threshold() - 1).copied().unwrap_or(0)
    }

    pub fn consensus_feerate(&self, dbtx: &impl picomint_redb::DbRead) -> Option<u64> {
        let num_peers = self.cfg.consensus.pks.to_num_peers();

        let mut rates: Vec<u64> =
            dbtx.iter(&FeeRateVoteTable, |r| r.filter_map(|(_, v)| v).collect());

        assert!(rates.len() <= num_peers.total());

        rates.sort_unstable();

        assert!(rates.first() <= rates.last());

        rates.get(num_peers.threshold() - 1).copied()
    }

    pub fn consensus_fee(
        &self,
        dbtx: &impl picomint_redb::DbRead,
        tx_vbytes: u64,
    ) -> Option<Amount> {
        // The minimum feerate is a protection against a catastrophic error in the
        // feerate estimation and limits the length of the pending transaction stack.

        let pending_txs = pending_txs_unordered(dbtx);

        assert!(pending_txs.len() <= 32);

        let feerate = self
            .consensus_feerate(dbtx)?
            .max(self.cfg.consensus.feerate_base << pending_txs.len());

        let tx_fee = tx_vbytes.saturating_mul(feerate).saturating_div(1000);

        let stack_vbytes = pending_txs
            .iter()
            .map(|t| t.vbytes)
            .try_fold(tx_vbytes, u64::checked_add)
            .expect("Stack vbytes overflow with at most 32 pending txs");

        let stack_fee = stack_vbytes.saturating_mul(feerate).saturating_div(1000);

        // Deduct the fees already paid by currently pending transactions
        let stack_fee = pending_txs
            .iter()
            .map(|t| t.fee.to_sat())
            .fold(stack_fee, u64::saturating_sub);

        Some(Amount::from_sat(tx_fee.max(stack_fee)))
    }

    pub fn send_fee(&self, dbtx: &impl picomint_redb::DbRead) -> Option<Amount> {
        self.consensus_fee(dbtx, self.cfg.consensus.send_tx_vbytes)
    }

    pub fn receive_fee(&self, dbtx: &impl picomint_redb::DbRead) -> Option<Amount> {
        self.consensus_fee(dbtx, self.cfg.consensus.receive_tx_vbytes)
    }

    fn script_pubkey(&self, tweak: &sha256::Hash) -> ScriptBuf {
        tweaked_script_pubkey(&self.cfg.consensus.agg_pk, tweak)
    }

    fn tweak_scalar(tweak: &sha256::Hash) -> Scalar {
        Scalar::from_be_bytes(tweak.to_byte_array()).expect("Hash is within field order")
    }

    fn tweaked_sks(&self, tweak: &sha256::Hash) -> SecretKeyShare {
        SecretKeyShare(
            self.cfg
                .private
                .sks
                .0
                .add_tweak(&Self::tweak_scalar(tweak))
                .expect("Failed to tweak wallet secret key share"),
        )
    }

    fn tweaked_pks(&self, peer: &PeerId, tweak: &sha256::Hash) -> PublicKeyShare {
        let pks = self
            .cfg
            .consensus
            .pks
            .get(peer)
            .expect("Failed to get public key share of peer from config");

        PublicKeyShare(tweak_public_key(&pks.0, tweak))
    }

    fn tweaked_agg_pk(&self, tweak: &sha256::Hash) -> AggregatePublicKey {
        AggregatePublicKey(tweak_public_key(&self.cfg.consensus.agg_pk.0, tweak))
    }

    /// The BIP341 keyspend sighash of every input of the transaction.
    fn sighashes(&self, unsigned_tx: &FederationTx) -> Vec<[u8; 32]> {
        let prevouts: Vec<TxOut> = unsigned_tx
            .spent_tx_outs
            .iter()
            .map(|utxo| TxOut {
                value: utxo.value,
                script_pubkey: self.script_pubkey(&utxo.tweak),
            })
            .collect();

        let mut sighash_cache = SighashCache::new(unsigned_tx.tx.clone());

        (0..unsigned_tx.spent_tx_outs.len())
            .map(|index| {
                sighash_cache
                    .taproot_key_spend_signature_hash(
                        index,
                        &Prevouts::All(&prevouts),
                        TapSighashType::Default,
                    )
                    .expect("Failed to compute taproot keyspend sighash")
                    .to_byte_array()
            })
            .collect()
    }

    fn sign_tx(
        &self,
        unsigned_tx: &FederationTx,
        sighashes: &[[u8; 32]],
        secret_nonces: Vec<SecretNonce>,
        chunk: &[NonceEntry],
    ) -> Vec<SignatureShare> {
        unsigned_tx
            .spent_tx_outs
            .iter()
            .zip(sighashes)
            .zip(secret_nonces)
            .enumerate()
            .map(|(index, ((utxo, msg), nonce))| {
                sign_share(
                    *msg,
                    &self.tweaked_sks(&utxo.tweak),
                    nonce,
                    &nonce_column(chunk, index),
                    self.identity.to_u64(),
                    &self.tweaked_agg_pk(&utxo.tweak),
                )
            })
            .collect()
    }

    fn finalize_tx(
        &self,
        federation_tx: &mut FederationTx,
        sighashes: &[[u8; 32]],
        chunk: &[NonceEntry],
        responses: &[Signatures],
    ) {
        assert_eq!(
            federation_tx.spent_tx_outs.len(),
            federation_tx.tx.input.len()
        );

        for (index, (utxo, msg)) in federation_tx
            .spent_tx_outs
            .iter()
            .zip(sighashes)
            .enumerate()
        {
            let shares: BTreeMap<u64, SignatureShare> = chunk
                .iter()
                .zip(responses)
                .map(|entry| {
                    let share = entry
                        .1
                        .0
                        .get(index)
                        .expect("Signature shares are validated to have one share per input");

                    (entry.0.0.to_u64(), *share)
                })
                .collect();

            let signature = aggregate_signature_shares(
                *msg,
                &nonce_column(chunk, index),
                &shares,
                &self.tweaked_agg_pk(&utxo.tweak),
            );

            assert!(
                tss::verify(*msg, &signature, &self.tweaked_agg_pk(&utxo.tweak)),
                "Aggregated signature failed verification"
            );

            federation_tx.tx.input[index].witness =
                Witness::p2tr_key_spend(&bitcoin::taproot::Signature {
                    signature,
                    sighash_type: TapSighashType::Default,
                });
        }
    }

    fn tx_id(dbtx: &impl picomint_redb::DbRead, outpoint: OutPoint) -> Option<Txid> {
        let index = dbtx.get(&TxInfoIndexTable, &outpoint)?;

        dbtx.get(&TxInfoTable, &index).map(|entry| entry.txid)
    }

    fn get_outputs(
        dbtx: &impl picomint_redb::DbRead,
        start_index: u64,
        end_index: u64,
    ) -> Vec<OutputInfo> {
        let spent: BTreeSet<u64> = dbtx.range(&SpentOutputTable, start_index..end_index, |r| {
            r.map(|(idx, ())| idx).collect()
        });

        dbtx.range(&OutputTable, start_index..end_index, |r| {
            r.filter_map(|(idx, Output(_, tx_out))| {
                tx_out.script_pubkey.is_p2tr().then(|| OutputInfo {
                    index: idx,
                    script: tx_out.script_pubkey,
                    value: tx_out.value,
                    spent: spent.contains(&idx),
                })
            })
            .collect()
        })
    }

    fn pending_tx_chain(dbtx: &impl picomint_redb::DbRead) -> Vec<TxInfo> {
        let n_pending = pending_txs_unordered(dbtx).len();

        let mut items: Vec<TxInfo> = dbtx.iter(&TxInfoTable, |r| r.map(|(_, v)| v).collect());

        items.reverse();
        items.truncate(n_pending);
        items
    }

    fn tx_chain(dbtx: &impl picomint_redb::DbRead) -> Vec<TxInfo> {
        dbtx.iter(&TxInfoTable, |r| r.map(|(_, v)| v).collect())
    }

    fn total_txs(dbtx: &impl picomint_redb::DbRead) -> u64 {
        dbtx.iter(&TxInfoTable, |r| r.next_back().map_or(0, |(k, _)| k + 1))
    }

    /// Get the network for UI display
    pub fn network_ui(&self) -> Network {
        self.network
    }

    /// Get the current federation wallet info for UI display
    pub fn federation_wallet_ui(&self) -> Option<FederationWallet> {
        self.db.begin_read().get(&FederationWalletTable, &())
    }

    /// Get the current consensus block count for UI display
    pub fn consensus_block_count_ui(&self) -> u64 {
        self.consensus_block_count(&self.db.begin_read())
    }

    /// Get the current consensus feerate for UI display
    pub fn consensus_feerate_ui(&self) -> Option<u64> {
        self.consensus_feerate(&self.db.begin_read())
            .map(|f| f / 1000)
    }

    /// Get the current send fee for UI display
    pub fn send_fee_ui(&self) -> Option<Amount> {
        self.send_fee(&self.db.begin_read())
    }

    /// Get the current receive fee for UI display
    pub fn receive_fee_ui(&self) -> Option<Amount> {
        self.receive_fee(&self.db.begin_read())
    }

    /// Get the current pending transaction info for UI display
    pub fn pending_tx_chain_ui(&self) -> Vec<TxInfo> {
        Self::pending_tx_chain(&self.db.begin_read())
    }

    /// Get the current transaction log for UI display
    pub fn tx_chain_ui(&self) -> Vec<TxInfo> {
        Self::tx_chain(&self.db.begin_read())
    }

    /// Get the total number of transactions in the chain for UI display
    pub fn transaction_count_ui(&self) -> u64 {
        Self::total_txs(&self.db.begin_read())
    }

    /// Export recovery material for federation shutdown: the tweaked
    /// aggregate public key and this guardian's tweaked secret key share.
    /// Additive tweaks commute with Lagrange interpolation, so an offline
    /// tool can interpolate any threshold of tweaked key shares directly
    /// into the secret key of the current federation UTXO, verify it
    /// against the tweaked aggregate key and sweep the UTXO with a
    /// single-key taproot wallet. Returns None if the federation wallet has
    /// not been initialized yet.
    pub fn restore_keys_ui(&self) -> Option<(String, String)> {
        let wallet = self.federation_wallet_ui()?;

        Some((
            self.tweaked_agg_pk(&wallet.tweak).0.to_string(),
            self.tweaked_sks(&wallet.tweak)
                .0
                .display_secret()
                .to_string(),
        ))
    }
}

/// The nonces of a signing session for a single tx input, keyed by the
/// signer indices the tss crate interpolates over.
fn nonce_column(chunk: &[NonceEntry], index: usize) -> BTreeMap<u64, PublicNonce> {
    chunk
        .iter()
        .map(|entry| {
            let nonce = entry
                .1
                .get(index)
                .expect("Nonce entries are validated to have one nonce per input");

            (entry.0.to_u64(), *nonce)
        })
        .collect()
}
