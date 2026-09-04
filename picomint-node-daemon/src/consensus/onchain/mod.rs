pub mod db;
mod rpc;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use self::db::{
    FeeRateVoteTable, MintOnchainTable, NonceEntry, NonceLogTable, Output, OutputTable,
    SignaturesTable, SpentOutputIndexTable, TxInfoIndexTable, TxInfoTable, UnconfirmedTxTable,
    UnsignedTxTable,
};
use crate::bitcoind::BitcoindRpcMonitor;
use anyhow::{Context, anyhow, ensure};
use bitcoin::absolute::LockTime;
use bitcoin::hashes::{Hash, sha256};
use bitcoin::sighash::{Prevouts, SighashCache, TapSighashType};
use bitcoin::transaction::Version;
use bitcoin::{Amount, Network, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness};
use common::config::OnchainConfigConsensus;
use common::{OnchainConsensusItem, OnchainInput, OnchainOutput, OutputInfo};
use picomint_core::backoff::{Retryable, networking_backoff};
use picomint_core::onchain as common;
use picomint_core::secp256k1::XOnlyPublicKey;
use picomint_core::{NodeId, NumNodesExt, OutPoint};
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::{Database, DbRead, ReadTx, WriteTx};
use tokio::time::sleep;

use crate::config::NodeConfig;
use crate::config::dkg::DkgHandle;
use crate::config::dkg_secp::eval_poly;
use crate::consensus::CONFIRMATION_FINALITY_DELAY;
use crate::consensus::db::consensus_block_count;
use crate::consensus::server::Server;
use crate::handler;
use picomint_core::onchain::config::{OnchainConfig, OnchainConfigPrivate};
use picomint_core::onchain::methods::OnchainMethod;
use picomint_core::onchain::{
    MintUtxo, OnchainInputError, OnchainOutputError, TxInfo, is_potential_receive,
    tweak_public_key, tweaked_script_pubkey,
};
use picomint_core::secret::Secret;
use secp256k1::Scalar;
use serde::{Deserialize, Serialize};
use tracing::info;
use tss::{
    AggregatePublicKey, PublicKeyShare, PublicNonce, SecretKeyShare, SecretNonce, SignatureShare,
    aggregate_signature_shares, derive_nonce, derive_pk_share, derive_public_nonce, sign_share,
    verify_signature_share,
};

/// Minimum fee rate vote of 1 sat/vB to ensure we never propose a fee rate
/// below what Bitcoin Core will relay.
const MIN_FEERATE_VOTE_SATS_PER_KVB: u32 = 1000;

// A mint tx is a taproot key spend whose witness is always exactly one
// 64-byte BIP340 signature, no matter how many nodes signed — so both tx
// shapes have a constant size known upfront. In BIP-141 weight units
// (non-witness bytes count 4, witness bytes 1): 42 overhead (nVersion 16,
// marker + flag 2, one-byte in/out count varints 8, nLockTime 16), 230 per
// input (txid 128, vout 16, empty scriptSig length 4, nSequence 16, witness
// 66), 172 per output with a 34-byte scriptPubKey (nValue 32, length 4,
// script 136). Both figures are verified exactly by the integration suite:
// the finalized taproot-destination pegout logs 154 vbytes and the second
// pegin's sweep logs 169 via the "Finalized mint tx" line.

/// A send spends the mint UTXO into a destination output and a change
/// output: 42 + 230 + 172 + 172 = 616 wu. Sized for the largest destination
/// script a [`StandardScript`] can carry (34 bytes, P2WSH/P2TR), so smaller
/// destinations are overcharged by up to 3 vbytes.
///
/// [`StandardScript`]: picomint_core::onchain::StandardScript
const SEND_TX_VBYTES: u64 = 154;

/// A receive sweeps the deposit and the mint UTXO into one change
/// output: 42 + 230 + 230 + 172 = 674 wu.
const RECEIVE_TX_VBYTES: u64 = 169;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Encodable, Decodable)]
pub struct MintTx {
    pub tx: Transaction,
    pub spent_tx_outs: Vec<SpentTxOut>,
    pub vbytes: u64,
    pub fee: Amount,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Encodable, Decodable)]
pub struct SpentTxOut {
    pub value: Amount,
    pub tweak: sha256::Hash,
}

fn pending_txs_unordered(dbtx: &impl DbRead) -> Vec<MintTx> {
    let unsigned: Option<MintTx> = dbtx.get(&UnsignedTxTable, &());

    let unconfirmed: Vec<MintTx> = dbtx.iter(&UnconfirmedTxTable, |r| r.map(|(_, v)| v).collect());

    unsigned.into_iter().chain(unconfirmed).collect()
}

/// Run DKG for the onchain module, producing a fresh `OnchainConfig` for this
/// node.
pub async fn dkg(nodes: &DkgHandle<'_>) -> anyhow::Result<OnchainConfig> {
    let (polynomial, sks) = nodes.run_dkg_secp().await?;

    let pks = nodes
        .num_nodes()
        .node_ids()
        .map(|node| Ok((node, PublicKeyShare(eval_poly(&polynomial, &node)?))))
        .collect::<anyhow::Result<BTreeMap<NodeId, PublicKeyShare>>>()?;

    Ok(OnchainConfig {
        private: OnchainConfigPrivate {
            sks: SecretKeyShare(sks),
        },
        consensus: OnchainConfigConsensus::new(AggregatePublicKey(polynomial[0]), pks),
    })
}

/// Verify our onchain secret key share matches the corresponding public key
/// share in the consensus config.
pub fn validate_config(cfg: &NodeConfig) -> anyhow::Result<()> {
    ensure!(
        cfg.consensus
            .onchain
            .pks
            .get(&cfg.private.identity)
            .context("Public key share set has no key for our identity")?
            == &derive_pk_share(&cfg.private.onchain.sks),
        "Onchain secret key share does not match our public key share"
    );

    Ok(())
}

pub fn consensus_proposal(server: &Server, dbtx: &ReadTx) -> Vec<OnchainConsensusItem> {
    let mut items: Vec<OnchainConsensusItem> = dbtx
        .get(&UnsignedTxTable, &())
        .and_then(|unsigned_tx| signing_session_proposal(server, dbtx, &unsigned_tx))
        .into_iter()
        .collect();

    let feerate_vote = server.btc_rpc.status().and_then(|status| {
        status
            .fee_rate
            .map(|fee_rate| fee_rate.sat_per_kvb.max(MIN_FEERATE_VOTE_SATS_PER_KVB))
    });

    // `None` retracts our vote while the bitcoin backend is down or
    // still syncing and thus unable to estimate fees.
    if dbtx.get(&FeeRateVoteTable, &server.cfg.private.identity) != Some(feerate_vote) {
        items.push(OnchainConsensusItem::Feerate(feerate_vote));
    }

    items
}

/// Determines the next item to propose for an unsigned transaction: our
/// signature shares plus a fresh replacement nonce entry if we are a
/// member of a signing session we have not signed yet, or our initial
/// nonce entry if we have never entered the log. Items are re-proposed
/// until they are accepted; the handlers reject duplicates.
fn signing_session_proposal(
    server: &Server,
    dbtx: &ReadTx,
    unsigned_tx: &MintTx,
) -> Option<OnchainConsensusItem> {
    let txid = unsigned_tx.tx.compute_txid();

    let inputs = unsigned_tx.spent_tx_outs.len();

    let latest = dbtx.iter_rev(&NonceLogTable, |r| {
        r.find(|entry| entry.1.0 == server.cfg.private.identity)
            .map(|entry| entry.0 as usize)
    });

    let Some(latest) = latest else {
        let nonces = derive_secret_nonces(server, txid, 0, inputs);

        let public_nonces = nonces.iter().map(derive_public_nonce).collect();

        return Some(OnchainConsensusItem::Nonces(txid, public_nonces));
    };

    let session = latest / threshold(server);

    // Our latest entry's session is always unsigned by us, since our
    // shares are stored in the same atomic step that appends our next
    // entry. If its chunk is still incomplete we idle; once it completes
    // we re-propose our deterministic shares until they are accepted.
    let chunk: Vec<NonceEntry> = dbtx.iter(&NonceLogTable, |r| {
        r.skip(session * threshold(server))
            .take(threshold(server))
            .map(|entry| entry.1)
            .collect()
    });

    if chunk.len() < threshold(server) {
        return None;
    }

    let sighashes = sighashes(server, unsigned_tx);

    let generation = dbtx.iter(&NonceLogTable, |r| {
        r.filter(|entry| entry.1.0 == server.cfg.private.identity)
            .count() as u64
            - 1
    });

    let nonces = derive_secret_nonces(server, txid, generation, inputs);

    let shares = sign_tx(server, unsigned_tx, &sighashes, nonces, &chunk);

    let fresh_nonces = derive_secret_nonces(server, txid, generation + 1, inputs);

    let public_nonces = fresh_nonces.iter().map(derive_public_nonce).collect();

    Some(OnchainConsensusItem::Signatures(
        txid,
        shares,
        public_nonces,
    ))
}

/// Derives the secret nonces for our nonce entry of the given generation,
/// where generation n is our n-th entry in the transaction's log.
/// Deterministic derivation is safe here because the log's total order
/// fixes each entry's signing session before any share is computed, the
/// message is fixed by the txid and a txid is never signed as a fresh
/// transaction twice - so every nonce meets exactly one signing context,
/// even across crash recovery.
fn derive_secret_nonces(
    server: &Server,
    txid: Txid,
    generation: u64,
    inputs: usize,
) -> Vec<SecretNonce> {
    let secret = Secret::new_root(&server.cfg.private.onchain.sks)
        .child(&txid)
        .child(&generation);

    (0..inputs as u64)
        .map(|input| derive_nonce(&secret.child(&input).to_byte_array()))
        .collect()
}

fn threshold(server: &Server) -> usize {
    server.cfg.consensus.onchain.pks.to_num_nodes().threshold()
}

pub async fn process_consensus_item(
    server: &Server,
    dbtx: &WriteTx,
    node: NodeId,
    consensus_item: OnchainConsensusItem,
) -> anyhow::Result<()> {
    match consensus_item {
        OnchainConsensusItem::Feerate(feerate) => {
            if Some(feerate) == dbtx.insert(&FeeRateVoteTable, &node, &feerate) {
                return Err(anyhow!("Fee rate vote is redundant"));
            }

            Ok(())
        }
        OnchainConsensusItem::Nonces(txid, nonces) => process_nonces(dbtx, node, txid, nonces),
        OnchainConsensusItem::Signatures(txid, shares, nonces) => {
            process_signatures(server, dbtx, node, txid, shares, nonces).await
        }
    }
}

pub fn process_input(
    server: &Server,
    dbtx: &WriteTx,
    input: &OnchainInput,
) -> Result<(picomint_core::Amount, XOnlyPublicKey), OnchainInputError> {
    if dbtx
        .insert(&SpentOutputIndexTable, &input.output_index, &())
        .is_some()
    {
        return Err(OnchainInputError::OutputAlreadySpent);
    }

    let Output(tracked_outpoint, tracked_output) = dbtx
        .get(&OutputTable, &input.output_index)
        .ok_or(OnchainInputError::UnknownOutputIndex)?;

    let tweaked_script = script_pubkey(server, &input.tweak.consensus_hash());

    if tracked_output.script_pubkey != tweaked_script {
        return Err(OnchainInputError::WrongTweak);
    }

    let consensus_receive_fee =
        receive_fee(server, dbtx).ok_or(OnchainInputError::NoConsensusFeerateAvailable)?;

    // We allow for a higher fee such that a node could construct a CPFP
    // transaction. This is the last line of defense should the mints
    // transactions ever get stuck due to a critical failure of the feerate
    // estimation.
    if input.fee < consensus_receive_fee {
        return Err(OnchainInputError::InsufficientTotalFee);
    }

    let output_value = tracked_output
        .value
        .checked_sub(input.fee)
        .ok_or(OnchainInputError::ArithmeticOverflow)?;

    if let Some(wallet) = dbtx.remove(&MintOnchainTable, &()) {
        // Assuming the first receive into the mint is made through a
        // standard transaction, its output value is over the dust limit.
        // By induction so is this change value.
        let change_value = wallet
            .value
            .checked_add(output_value)
            .ok_or(OnchainInputError::ArithmeticOverflow)?;

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
                script_pubkey: script_pubkey(server, &wallet.consensus_hash()),
            }],
        };

        dbtx.insert(
            &MintOnchainTable,
            &(),
            &MintUtxo {
                value: change_value,
                outpoint: bitcoin::OutPoint {
                    txid: tx.compute_txid(),
                    vout: 0,
                },
                tweak: wallet.consensus_hash(),
            },
        );

        let tx_index = total_txs(dbtx);

        let created = consensus_block_count(server, dbtx);

        dbtx.insert(
            &TxInfoTable,
            &tx_index,
            &TxInfo {
                index: tx_index,
                txid: tx.compute_txid(),
                input: wallet.value,
                output: change_value,
                vbytes: RECEIVE_TX_VBYTES,
                fee: input.fee,
                created,
            },
        );

        let unsigned_tx = MintTx {
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
            vbytes: RECEIVE_TX_VBYTES,
            fee: input.fee,
        };

        if dbtx.insert(&UnsignedTxTable, &(), &unsigned_tx).is_some() {
            return Err(OnchainInputError::PendingTransaction);
        }
    } else {
        dbtx.insert(
            &MintOnchainTable,
            &(),
            &MintUtxo {
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
        .ok_or(OnchainInputError::ArithmeticOverflow)?;

    Ok((amount, input.tweak))
}

pub fn process_output(
    server: &Server,
    dbtx: &WriteTx,
    output: &OnchainOutput,
    outpoint: OutPoint,
) -> Result<picomint_core::Amount, OnchainOutputError> {
    if output.value < server.cfg.consensus.onchain.dust_limit {
        return Err(OnchainOutputError::UnderDustLimit);
    }

    let wallet = dbtx
        .remove(&MintOnchainTable, &())
        .ok_or(OnchainOutputError::NoMintUtxo)?;

    let consensus_send_fee =
        send_fee(server, dbtx).ok_or(OnchainOutputError::NoConsensusFeerateAvailable)?;

    // We allow for a higher fee such that a node could construct a CPFP
    // transaction. This is the last line of defense should the mints
    // transactions ever get stuck due to a critical failure of the feerate
    // estimation.
    if output.fee < consensus_send_fee {
        return Err(OnchainOutputError::InsufficientTotalFee);
    }

    let output_value = output
        .value
        .checked_add(output.fee)
        .ok_or(OnchainOutputError::ArithmeticOverflow)?;

    let change_value = wallet
        .value
        .checked_sub(output_value)
        .ok_or(OnchainOutputError::ArithmeticOverflow)?;

    if change_value < server.cfg.consensus.onchain.dust_limit {
        return Err(OnchainOutputError::ChangeUnderDustLimit);
    }

    let script_pubkey_out = output.destination.script_pubkey();

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
                script_pubkey: script_pubkey(server, &wallet.consensus_hash()),
            },
            TxOut {
                value: output.value,
                script_pubkey: script_pubkey_out,
            },
        ],
    };

    dbtx.insert(
        &MintOnchainTable,
        &(),
        &MintUtxo {
            value: change_value,
            outpoint: bitcoin::OutPoint {
                txid: tx.compute_txid(),
                vout: 0,
            },
            tweak: wallet.consensus_hash(),
        },
    );

    let tx_index = total_txs(dbtx);

    let created = consensus_block_count(server, dbtx);

    dbtx.insert(
        &TxInfoTable,
        &tx_index,
        &TxInfo {
            index: tx_index,
            txid: tx.compute_txid(),
            input: wallet.value,
            output: change_value,
            vbytes: SEND_TX_VBYTES,
            fee: output.fee,
            created,
        },
    );

    dbtx.insert(&TxInfoIndexTable, &outpoint, &tx_index);

    let unsigned_tx = MintTx {
        tx: tx.clone(),
        spent_tx_outs: vec![SpentTxOut {
            value: wallet.value,
            tweak: wallet.tweak,
        }],
        vbytes: SEND_TX_VBYTES,
        fee: output.fee,
    };

    if dbtx.insert(&UnsignedTxTable, &(), &unsigned_tx).is_some() {
        return Err(OnchainOutputError::PendingTransaction);
    }

    output_value
        .to_sat()
        .checked_mul(1000)
        .map(picomint_core::Amount::from_msat)
        .ok_or(OnchainOutputError::ArithmeticOverflow)
}

pub fn audit(dbtx: &WriteTx) -> i64 {
    dbtx.get(&MintOnchainTable, &())
        .map_or(0, |wallet| 1000 * wallet.value.to_sat() as i64)
}

pub async fn handle_api(server: &Server, method: OnchainMethod) -> Result<Vec<u8>, String> {
    match method {
        OnchainMethod::ConsensusFeerate(req) => handler!(consensus_feerate, server, req).await,
        OnchainMethod::MintUtxo(req) => handler!(mint_utxo, server, req).await,
        OnchainMethod::SendFee(req) => handler!(send_fee, server, req).await,
        OnchainMethod::ReceiveFee(req) => handler!(receive_fee, server, req).await,
        OnchainMethod::TxId(req) => handler!(tx_id, server, req).await,
        OnchainMethod::OutputInfoSlice(req) => handler!(output_info_slice, server, req).await,
        OnchainMethod::PendingTxChain(req) => handler!(pending_tx_chain, server, req).await,
        OnchainMethod::TxChain(req) => handler!(tx_chain, server, req).await,
    }
}

pub fn spawn_broadcast_unconfirmed_txs_task(
    btc_rpc: BitcoindRpcMonitor,
    db: Database,
    network: Network,
) {
    tokio::spawn(async move {
        loop {
            let unconfirmed_txs: Vec<MintTx> = db
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

/// Scan the blocks the consensus block count advanced over for pegins and
/// confirmations of the mint's own transactions. Called by the
/// consensus engine whenever the consensus block count advances.
pub async fn sync_blocks(
    server: &Server,
    dbtx: &WriteTx,
    old_block_count: u32,
    new_block_count: u32,
) {
    // We do not sync blocks that predate the mint itself.
    if old_block_count == 0 {
        return;
    }

    // Our bitcoin backend needs to be synced for the following calls to the
    // get_block rpc to be safe for consensus.
    await_local_sync_to_block_count(server, new_block_count + CONFIRMATION_FINALITY_DELAY).await;

    for height in old_block_count..new_block_count {
        let block_hash = (|| server.btc_rpc.get_block_hash(height))
            .retry(networking_backoff())
            .await
            .expect("networking_backoff retries forever");

        let block = (|| server.btc_rpc.get_block(&block_hash))
            .retry(networking_backoff())
            .await
            .expect("networking_backoff retries forever");

        assert_eq!(block.block_hash(), block_hash, "Block hash mismatch");

        let pks_hash = server.cfg.consensus.onchain.agg_pk.consensus_hash();

        for tx in block.txdata {
            dbtx.remove(&UnconfirmedTxTable, &tx.compute_txid());

            // We maintain an append-only log of transaction outputs that pass
            // the probabilistic receive filter created since the mint was
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
                        dbtx.iter_rev(&OutputTable, |r| r.next().map_or(0, |entry| entry.0 + 1));

                    dbtx.insert(&OutputTable, &index, &Output(outpoint, tx_out.clone()));
                }
            }
        }
    }
}

fn process_nonces(
    dbtx: &WriteTx,
    node: NodeId,
    txid: Txid,
    nonces: Vec<PublicNonce>,
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
        dbtx.iter(&NonceLogTable, |r| r.all(|entry| entry.1.0 != node)),
        "Nonce entry is redundant"
    );

    let next_index = dbtx.iter_rev(&NonceLogTable, |r| r.next().map_or(0, |entry| entry.0 + 1));

    dbtx.insert(&NonceLogTable, &next_index, &NonceEntry(node, nonces));

    Ok(())
}

async fn process_signatures(
    server: &Server,
    dbtx: &WriteTx,
    node: NodeId,
    txid: Txid,
    shares: Vec<SignatureShare>,
    fresh_nonces: Vec<PublicNonce>,
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

    // The session of the node's latest entry is the only one it might
    // not have signed yet, since appending an entry requires signing the
    // session of the previous one.
    let latest = dbtx
        .iter_rev(&NonceLogTable, |r| {
            r.find(|entry| entry.1.0 == node)
                .map(|entry| entry.0 as usize)
        })
        .context("Node has no nonce entry")?;

    let session = latest / threshold(server);

    let chunk_range =
        (session * threshold(server)) as u64..((session + 1) * threshold(server)) as u64;

    let chunk: Vec<NonceEntry> = dbtx.range(&NonceLogTable, chunk_range.clone(), |r| {
        r.map(|entry| entry.1).collect()
    });

    ensure!(
        chunk.len() == threshold(server),
        "The signing session of the node's latest entry is still forming"
    );

    let sighashes = sighashes(server, &unsigned);

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
                node.to_u64(),
                &tweaked_pks(server, &node, &utxo.tweak),
                share,
                &nonce_column(&chunk, index),
                &tweaked_agg_pk(server, &utxo.tweak),
            ),
            "Invalid signature share"
        );
    }

    ensure!(
        dbtx.insert(&SignaturesTable, &(latest as u64), &shares)
            .is_none(),
        "Already received signature shares for this entry"
    );

    let next_index = dbtx.iter_rev(&NonceLogTable, |r| r.next().map_or(0, |entry| entry.0 + 1));

    dbtx.insert(&NonceLogTable, &next_index, &NonceEntry(node, fresh_nonces));

    let responses: Vec<Vec<SignatureShare>> = dbtx.range(&SignaturesTable, chunk_range, |r| {
        r.map(|(_, shares)| shares).collect()
    });

    if responses.len() == threshold(server) {
        finalize_tx(server, &mut unsigned, &sighashes, &chunk, &responses);

        dbtx.remove(&UnsignedTxTable, &());

        dbtx.clear_table(&NonceLogTable);

        dbtx.clear_table(&SignaturesTable);

        dbtx.insert(&UnconfirmedTxTable, &txid, &unsigned);

        server.btc_rpc.submit_tx(unsigned.tx).await;
    }

    Ok(())
}

async fn await_local_sync_to_block_count(server: &Server, block_count: u32) {
    loop {
        if server
            .btc_rpc
            .status()
            .is_some_and(|status| status.block_count >= block_count)
        {
            break;
        }

        info!("Waiting for local bitcoin backend to sync to block count {block_count}");

        if server.cfg.consensus.network == Network::Regtest {
            sleep(Duration::from_secs(1)).await;
        } else {
            sleep(Duration::from_secs(60)).await;
        }
    }
}

pub fn consensus_feerate(server: &Server, dbtx: &impl DbRead) -> Option<u32> {
    let num_nodes = server.cfg.consensus.onchain.pks.to_num_nodes();

    let mut rates: Vec<u32> = dbtx.iter(&FeeRateVoteTable, |r| r.filter_map(|(_, v)| v).collect());

    assert!(rates.len() <= num_nodes.total());

    rates.sort_unstable();

    assert!(rates.first() <= rates.last());

    rates.get(num_nodes.threshold() - 1).copied()
}

pub fn consensus_fee(server: &Server, dbtx: &impl DbRead, tx_vbytes: u64) -> Option<Amount> {
    // The minimum feerate is a protection against a catastrophic error in the
    // feerate estimation and limits the length of the pending transaction stack.

    let pending_txs = pending_txs_unordered(dbtx);

    assert!(pending_txs.len() <= 32);

    let feerate = u64::from(consensus_feerate(server, dbtx)?)
        .max(u64::from(server.cfg.consensus.onchain.feerate_base) << pending_txs.len());

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

pub fn send_fee(server: &Server, dbtx: &impl DbRead) -> Option<Amount> {
    consensus_fee(server, dbtx, SEND_TX_VBYTES)
}

pub fn receive_fee(server: &Server, dbtx: &impl DbRead) -> Option<Amount> {
    consensus_fee(server, dbtx, RECEIVE_TX_VBYTES)
}

fn script_pubkey(server: &Server, tweak: &sha256::Hash) -> ScriptBuf {
    tweaked_script_pubkey(&server.cfg.consensus.onchain.agg_pk, tweak)
}

fn tweak_scalar(tweak: &sha256::Hash) -> Scalar {
    Scalar::from_be_bytes(tweak.to_byte_array()).expect("Hash is within field order")
}

fn tweaked_sks(server: &Server, tweak: &sha256::Hash) -> SecretKeyShare {
    SecretKeyShare(
        server
            .cfg
            .private
            .onchain
            .sks
            .0
            .add_tweak(&tweak_scalar(tweak))
            .expect("Failed to tweak wallet secret key share"),
    )
}

fn tweaked_pks(server: &Server, node: &NodeId, tweak: &sha256::Hash) -> PublicKeyShare {
    let pks = server
        .cfg
        .consensus
        .onchain
        .pks
        .get(node)
        .expect("Failed to get public key share of node from config");

    PublicKeyShare(tweak_public_key(&pks.0, tweak))
}

fn tweaked_agg_pk(server: &Server, tweak: &sha256::Hash) -> AggregatePublicKey {
    AggregatePublicKey(tweak_public_key(
        &server.cfg.consensus.onchain.agg_pk.0,
        tweak,
    ))
}

/// The BIP341 keyspend sighash of every input of the transaction.
fn sighashes(server: &Server, unsigned_tx: &MintTx) -> Vec<[u8; 32]> {
    let prevouts: Vec<TxOut> = unsigned_tx
        .spent_tx_outs
        .iter()
        .map(|utxo| TxOut {
            value: utxo.value,
            script_pubkey: script_pubkey(server, &utxo.tweak),
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
    server: &Server,
    unsigned_tx: &MintTx,
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
                &tweaked_sks(server, &utxo.tweak),
                nonce,
                &nonce_column(chunk, index),
                server.cfg.private.identity.to_u64(),
                &tweaked_agg_pk(server, &utxo.tweak),
            )
        })
        .collect()
}

fn finalize_tx(
    server: &Server,
    mint_tx: &mut MintTx,
    sighashes: &[[u8; 32]],
    chunk: &[NonceEntry],
    responses: &[Vec<SignatureShare>],
) {
    assert_eq!(mint_tx.spent_tx_outs.len(), mint_tx.tx.input.len());

    for (index, (utxo, msg)) in mint_tx.spent_tx_outs.iter().zip(sighashes).enumerate() {
        let shares: BTreeMap<u64, SignatureShare> = chunk
            .iter()
            .zip(responses)
            .map(|entry| {
                let share = entry
                    .1
                    .get(index)
                    .expect("Signature shares are validated to have one share per input");

                (entry.0.0.to_u64(), *share)
            })
            .collect();

        let signature = aggregate_signature_shares(
            *msg,
            &nonce_column(chunk, index),
            &shares,
            &tweaked_agg_pk(server, &utxo.tweak),
        );

        assert!(
            tss::verify(*msg, &signature, &tweaked_agg_pk(server, &utxo.tweak)),
            "Aggregated signature failed verification"
        );

        mint_tx.tx.input[index].witness = Witness::p2tr_key_spend(&bitcoin::taproot::Signature {
            signature,
            sighash_type: TapSighashType::Default,
        });
    }

    info!(
        inputs = mint_tx.tx.input.len(),
        outputs = mint_tx.tx.output.len(),
        vbytes = mint_tx.tx.vsize(),
        "Finalized mint tx"
    );
}

fn tx_id(dbtx: &impl DbRead, outpoint: OutPoint) -> Option<Txid> {
    let index = dbtx.get(&TxInfoIndexTable, &outpoint)?;

    dbtx.get(&TxInfoTable, &index).map(|entry| entry.txid)
}

fn get_outputs(dbtx: &impl DbRead, start_index: u64, end_index: u64) -> Vec<OutputInfo> {
    let spent: BTreeSet<u64> = dbtx.range(&SpentOutputIndexTable, start_index..end_index, |r| {
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

pub fn pending_tx_chain(dbtx: &impl DbRead) -> Vec<TxInfo> {
    let n_pending = pending_txs_unordered(dbtx).len();

    let mut items: Vec<TxInfo> = dbtx.iter(&TxInfoTable, |r| r.map(|(_, v)| v).collect());

    items.reverse();
    items.truncate(n_pending);
    items
}

pub fn tx_chain(dbtx: &impl DbRead) -> Vec<TxInfo> {
    dbtx.iter(&TxInfoTable, |r| r.map(|(_, v)| v).collect())
}

pub fn total_txs(dbtx: &impl DbRead) -> u64 {
    dbtx.iter_rev(&TxInfoTable, |r| r.next().map_or(0, |entry| entry.0 + 1))
}

/// The current mint wallet, if a first receive has established one.
pub fn mint_utxo(dbtx: &impl DbRead) -> Option<MintUtxo> {
    dbtx.get(&MintOnchainTable, &())
}

/// Export recovery material for mint shutdown: the tweaked
/// aggregate public key and this node's tweaked secret key share.
/// Additive tweaks commute with Lagrange interpolation, so an offline
/// tool can interpolate any threshold of tweaked key shares directly
/// into the secret key of the current mint UTXO, verify it
/// against the tweaked aggregate key and sweep the UTXO with a
/// single-key taproot wallet. Returns None if the mint wallet has
/// not been initialized yet.
pub fn restore_keys(server: &Server, dbtx: &impl DbRead) -> Option<(String, String)> {
    let wallet = mint_utxo(dbtx)?;

    Some((
        tweaked_agg_pk(server, &wallet.tweak).0.to_string(),
        tweaked_sks(server, &wallet.tweak)
            .0
            .display_secret()
            .to_string(),
    ))
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
