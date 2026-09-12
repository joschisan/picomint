//! The ecash rows of the analytics mirror: one per note spent and one
//! per note issued, keyed by the transaction and the position in it.
//! Outstanding notes per denomination is outputs minus inputs.

use picomint_core::TransactionId;
use picomint_core::ecash::{Denomination, EcashInput, EcashOutput};
use picomint_core::secp256k1::XOnlyPublicKey;
use picomint_core::sql::SqlRow;
use tbs::BlindedNonce;

use crate::consensus::analytics::Table;

/// A note spent as a transaction input.
#[derive(SqlRow)]
pub struct InputRow {
    pub txid: TransactionId,
    pub position: u16,
    pub denomination: Denomination,
    pub nonce: XOnlyPublicKey,
}

impl Table for InputRow {
    const NAME: &'static str = "ecash_input";
    const INDEXES: &'static [&'static str] = &["txid", "denomination"];
}

/// A note issued as a transaction output.
#[derive(SqlRow)]
pub struct OutputRow {
    pub txid: TransactionId,
    pub position: u16,
    pub denomination: Denomination,
    pub nonce: BlindedNonce,
}

impl Table for OutputRow {
    const NAME: &'static str = "ecash_output";
    const INDEXES: &'static [&'static str] = &["txid", "denomination"];
}

pub fn input_row(txid: TransactionId, position: u16, input: &EcashInput) -> InputRow {
    InputRow {
        txid,
        position,
        denomination: input.note.denomination,
        nonce: input.note.nonce,
    }
}

pub fn output_row(txid: TransactionId, position: u16, output: &EcashOutput) -> OutputRow {
    OutputRow {
        txid,
        position,
        denomination: output.denomination,
        nonce: output.nonce,
    }
}
