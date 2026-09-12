//! The onchain rows of the analytics mirror: deposits claimed and
//! withdrawals requested as transaction inputs and outputs, and the
//! module's own consensus items, one table per kind.

use bitcoin::{Address, Network, Txid};
use picomint_core::TransactionId;
use picomint_core::onchain::{OnchainConsensusItem, OnchainInput, OnchainOutput};
use picomint_core::secp256k1::XOnlyPublicKey;
use picomint_core::sql::{Row, SqlRow};

use crate::consensus::analytics::{Entry, Table, row};

/// A deposit claimed as a transaction input.
#[derive(SqlRow)]
pub struct InputRow {
    pub txid: TransactionId,
    pub position: u16,
    /// The tracked output the deposit landed in.
    pub output_index: u64,
    pub tweak: XOnlyPublicKey,
    pub fee_sat: bitcoin::Amount,
}

impl Table for InputRow {
    const NAME: &'static str = "onchain_input";
    const INDEXES: &'static [&'static str] = &["txid"];
}

/// A withdrawal requested as a transaction output.
#[derive(SqlRow)]
pub struct OutputRow {
    pub txid: TransactionId,
    pub position: u16,
    pub destination: String,
    pub value_sat: bitcoin::Amount,
    pub fee_sat: bitcoin::Amount,
}

impl Table for OutputRow {
    const NAME: &'static str = "onchain_output";
    const INDEXES: &'static [&'static str] = &["txid"];
}

/// A node's vote for the block at the tracked height, with how many
/// mint transactions it saw in it.
#[derive(SqlRow)]
pub struct BlockVoteRow {
    pub height: u32,
    pub txs: u64,
}

impl Table for BlockVoteRow {
    const NAME: &'static str = "onchain_block_vote";
    const INDEXES: &'static [&'static str] = &["node", "height"];
}

/// A node's feerate estimate in sat/kvB, null while its backend cannot
/// estimate. The consensus feerate is the threshold-th lowest of each
/// node's latest estimate.
#[derive(SqlRow)]
pub struct FeerateVoteRow {
    pub feerate: Option<u32>,
}

impl Table for FeerateVoteRow {
    const NAME: &'static str = "onchain_feerate_vote";
    const INDEXES: &'static [&'static str] = &["node"];
}

/// A node's entry into a pending transaction's nonce log.
#[derive(SqlRow)]
pub struct NoncesRow {
    pub btc_txid: Txid,
    pub nonces: u64,
}

impl Table for NoncesRow {
    const NAME: &'static str = "onchain_nonces";
    const INDEXES: &'static [&'static str] = &["btc_txid"];
}

/// A node's signature shares for a pending transaction's signing
/// session, with the replacement nonces it appended.
#[derive(SqlRow)]
pub struct SignatureSharesRow {
    pub btc_txid: Txid,
    pub shares: u64,
    pub nonces: u64,
}

impl Table for SignatureSharesRow {
    const NAME: &'static str = "onchain_signature_shares";
    const INDEXES: &'static [&'static str] = &["btc_txid"];
}

pub fn input_row(txid: TransactionId, position: u16, input: &OnchainInput) -> InputRow {
    InputRow {
        txid,
        position,
        output_index: input.output_index,
        tweak: input.tweak,
        fee_sat: input.fee,
    }
}

pub fn output_row(
    network: Network,
    txid: TransactionId,
    position: u16,
    output: &OnchainOutput,
) -> OutputRow {
    OutputRow {
        txid,
        position,
        destination: Address::from_script(&output.destination.script_pubkey(), network)
            .expect("A standard script has an address")
            .to_string(),
        value_sat: output.value,
        fee_sat: output.fee,
    }
}

pub fn item_row(entry: &Entry, item: &OnchainConsensusItem) -> Row {
    match item {
        OnchainConsensusItem::Block(vote) => row(
            entry,
            &BlockVoteRow {
                height: vote.height,
                txs: vote.txs.len() as u64,
            },
        ),
        OnchainConsensusItem::Feerate(feerate) => row(entry, &FeerateVoteRow { feerate: *feerate }),
        OnchainConsensusItem::Nonces(btc_txid, nonces) => row(
            entry,
            &NoncesRow {
                btc_txid: *btc_txid,
                nonces: nonces.len() as u64,
            },
        ),
        OnchainConsensusItem::SignatureShares(btc_txid, shares, nonces) => row(
            entry,
            &SignatureSharesRow {
                btc_txid: *btc_txid,
                shares: shares.len() as u64,
                nonces: nonces.len() as u64,
            },
        ),
    }
}
