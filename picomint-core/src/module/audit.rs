use serde::{Deserialize, Serialize};

/// Per-module + total net-asset snapshot, all in signed msat.
///
/// `total` is the sum of the three module fields and must never drop below
/// zero — that's the mint's balance-sheet invariant, checked on every
/// accepted transaction by the consensus engine.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AuditSummary {
    pub ecash: i64,
    pub onchain: i64,
    pub lightning: i64,
    pub total: i64,
}

impl AuditSummary {
    pub fn new(ecash: i64, onchain: i64, lightning: i64) -> Self {
        let total = ecash
            .checked_add(onchain)
            .and_then(|s| s.checked_add(lightning))
            .expect("Overflow while summing the mint's balance sheet");
        Self {
            ecash,
            onchain,
            lightning,
            total,
        }
    }
}
