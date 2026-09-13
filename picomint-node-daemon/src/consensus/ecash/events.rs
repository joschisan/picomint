//! The ecash events: one per note spent as an input and one per note
//! issued as an output, by denomination. Notes in circulation per
//! denomination is outputs minus inputs, which is the liability the
//! wallet's custody has to cover.

use picomint_core::ecash::Denomination;
use picomint_core::sql::SqlRow;
use picomint_core::{InPoint, OutPoint};
use serde::{Deserialize, Serialize};

use crate::consensus::eventlog::Event;

/// A note spent as a transaction input.
#[derive(Debug, Serialize, Deserialize, SqlRow)]
pub struct InputEvent {
    pub inpoint: InPoint,
    /// The note's value is `2^denomination` msat
    pub denomination: Denomination,
}

impl Event for InputEvent {
    const KIND: &'static str = "ecash-input";
}

/// A note issued as a transaction output.
#[derive(Debug, Serialize, Deserialize, SqlRow)]
pub struct OutputEvent {
    pub outpoint: OutPoint,
    /// The note's value is `2^denomination` msat
    pub denomination: Denomination,
}

impl Event for OutputEvent {
    const KIND: &'static str = "ecash-output";
}
