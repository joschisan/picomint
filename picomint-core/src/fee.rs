//! A cut taken on the transactions a client builds, and where to send it.
//!
//! Two parties can charge one. An integrator hands its own [`FeeConfig`] to
//! the client it builds; a federation announces one that every client of it
//! reads. Both are the same shape and both are collected the same way — the
//! cut accrues in an account of its own and the client pays it out over
//! Lightning — so this type describes either, and `Option<FeeConfig>` is how
//! both say "no cut" without a second spelling for it.

use picomint_encoding::{Decodable, Encodable};
use serde::{Deserialize, Serialize};

/// What to charge, and who to pay it to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Encodable, Decodable)]
pub struct FeeConfig {
    /// Parts per million of the value a transaction moves.
    pub ppm: u64,
    /// Where the collected cut is paid out, as a bech32 LNURL.
    ///
    /// Compared byte for byte when a federation announces one, so guardians
    /// have to agree on the spelling and not merely the destination — the
    /// same rule that governs the expiry announcement.
    pub lnurl: String,
}

picomint_redb::consensus_value!(FeeConfig);
