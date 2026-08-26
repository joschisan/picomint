//! A cut an integrator charges on the transactions its client builds, and
//! where to send it.
//!
//! Client-side throughout: the federation neither sets this nor is told it,
//! and cannot tell the outputs paying it from any other. `Option<FeeConfig>`
//! is how an integrator says it charges nothing.

use serde::{Deserialize, Serialize};

/// What to charge, and who to pay it to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeeConfig {
    /// Parts per million of the value a transaction moves.
    pub ppm: u64,
    /// Where the collected cut is paid out, as a bech32 LNURL.
    pub lnurl: String,
}
