use std::collections::{BTreeMap, BTreeSet};

use bitcoin_hashes::hash160;
use picomint_core::core::OperationId;
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::table;

use super::SpendableNote;
use super::issuance::NoteIssuanceRequest;

// Tracks that a `receive(ecash)` has been started for this deterministic
// [`OperationId`]. Used to make `receive` idempotent.
table!(
    RECEIVE_OPERATION,
    OperationId => (),
    "mint-receive-operation",
);

table!(
    NOTE,
    SpendableNote => (),
    "mint-note",
);

/// Mint recovery checkpoint — single in-flight recovery per client.
#[derive(Debug, Clone, Encodable, Decodable)]
pub struct Recovery {
    /// Op-id under which every progress / completion event is logged.
    /// Picked by `init_recovery` and persisted so a restart's driver
    /// keeps emitting under the same operation id and a UI subscriber doesn't
    /// see the stream split.
    pub operation: OperationId,
    /// Next item index to download
    pub next_index: u64,
    /// Total items (for progress calculation). `None` after
    /// `init_recovery` — the driver fills it in on its first awakening
    /// via `module_api.recovery_count()` so `init_recovery` doesn't
    /// have to hit the network.
    pub total_items: Option<u64>,
    /// Already recovered note requests, keyed by `nonce_hash` (for efficient
    /// removal when inputs are seen)
    pub requests: BTreeMap<hash160::Hash, NoteIssuanceRequest>,
    /// Nonces seen (to detect duplicates)
    pub nonces: BTreeSet<hash160::Hash>,
}

picomint_redb::consensus_value!(Recovery);

table!(
    RECOVERY,
    () => Recovery,
    "mint-recovery",
);
