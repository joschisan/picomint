//! The onchain events: a deposit claimed as an input and a withdrawal
//! requested as an output, the wallet's own transactions from creation to
//! confirmation, the blocks the mint tracked with their outputs, and the
//! consensus feerate moving. Amounts are sat.

use bitcoin::Txid;
use picomint_core::sql::SqlRow;
use picomint_core::{InPoint, OutPoint};
use serde::{Deserialize, Serialize};

use crate::consensus::eventlog::Event;

/// A deposit claimed as a transaction input: the tracked output it
/// swept, what it was worth and what the claim paid the miners.
#[derive(Debug, Serialize, Deserialize, SqlRow)]
pub struct InputEvent {
    pub inpoint: InPoint,
    /// The tracked output, `output_index` in `onchain_tracked_output`
    pub output_index: u64,
    pub btc_txid: Txid,
    pub vout: u32,
    pub value_sat: bitcoin::Amount,
    pub fee_sat: bitcoin::Amount,
    /// The wallet transaction that sweeps it, `btc_txid` in `onchain_tx`; null
    /// for the mint's first deposit, which becomes the wallet as it is
    pub wallet_txid: Option<Txid>,
}

impl Event for InputEvent {
    const KIND: &'static str = "onchain-input";
}

/// A withdrawal requested as a transaction output.
#[derive(Debug, Serialize, Deserialize, SqlRow)]
pub struct OutputEvent {
    pub outpoint: OutPoint,
    pub destination: String,
    pub value_sat: bitcoin::Amount,
    pub fee_sat: bitcoin::Amount,
    /// The wallet transaction that pays it, `btc_txid` in `onchain_tx`
    pub wallet_txid: Txid,
}

impl Event for OutputEvent {
    const KIND: &'static str = "onchain-output";
}

/// A wallet transaction created: the one that settles the deposit or
/// withdrawal accepted right before it. `input_sat` is the mint's custody
/// before it and `output_sat` after.
#[derive(Debug, Serialize, Deserialize, SqlRow)]
pub struct TxEvent {
    /// Position in the wallet's transaction chain, 0-based
    pub tx_index: u64,
    pub btc_txid: Txid,
    pub input_sat: bitcoin::Amount,
    pub output_sat: bitcoin::Amount,
    pub fee_sat: bitcoin::Amount,
}

impl Event for TxEvent {
    const KIND: &'static str = "onchain-tx";
}

/// A wallet transaction signed by a threshold of nodes and broadcast.
#[derive(Debug, Serialize, Deserialize, SqlRow)]
pub struct TxSignedEvent {
    pub btc_txid: Txid,
}

impl Event for TxSignedEvent {
    const KIND: &'static str = "onchain-tx-signed";
}

/// A wallet transaction seen in a tracked block.
#[derive(Debug, Serialize, Deserialize, SqlRow)]
pub struct TxConfirmedEvent {
    pub btc_txid: Txid,
    pub height: u32,
}

impl Event for TxConfirmedEvent {
    const KIND: &'static str = "onchain-tx-confirmed";
}

/// A block tracked: a threshold of nodes voted the same content for it.
#[derive(Debug, Serialize, Deserialize, SqlRow)]
pub struct BlockEvent {
    pub height: u32,
    /// Transactions with outputs to the mint or of the wallet's own
    pub txs: u64,
}

impl Event for BlockEvent {
    const KIND: &'static str = "onchain-block";
}

/// An output to the mint in a tracked block: a deposit waiting to be
/// claimed, or the wallet's own change. Claimed by the `onchain_input`
/// event naming its `output_index`.
#[derive(Debug, Serialize, Deserialize, SqlRow)]
pub struct TrackedOutputEvent {
    pub output_index: u64,
    pub height: u32,
    pub btc_txid: Txid,
    pub vout: u32,
    pub value_sat: bitcoin::Amount,
}

impl Event for TrackedOutputEvent {
    const KIND: &'static str = "onchain-tracked-output";
}

/// The consensus feerate moved: the threshold-th lowest of each node's
/// estimate, in sat/kvB, null while too few nodes can estimate.
#[derive(Debug, Serialize, Deserialize, SqlRow)]
pub struct FeerateEvent {
    pub feerate: Option<u32>,
}

impl Event for FeerateEvent {
    const KIND: &'static str = "onchain-feerate";
}
