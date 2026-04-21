use serde::{Deserialize, Serialize};

/// Per-module + total net-asset snapshot, all in signed msats.
///
/// `total` is the sum of the three module fields and must never drop below
/// zero — that's the federation's balance-sheet invariant, checked on every
/// accepted transaction in `ConsensusEngine`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AuditSummary {
    pub mint: i64,
    pub ln: i64,
    pub wallet: i64,
    pub total: i64,
}

impl AuditSummary {
    pub fn new(mint: i64, ln: i64, wallet: i64) -> Self {
        let total = mint
            .checked_add(ln)
            .and_then(|s| s.checked_add(wallet))
            .expect("Overflow while summing the federation's balance sheet");
        Self {
            mint,
            ln,
            wallet,
            total,
        }
    }
}
