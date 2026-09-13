//! Mint-wide expiry announcement. Nodes can collectively
//! declare a future shutdown date (and optionally a successor mint's
//! invite code) that clients fetch via threshold consensus and surface to
//! their users.

use picomint_encoding::{Decodable, Encodable};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::invite::InviteCode;

/// A mint's expiry announcement: the date it winds down and, optionally,
/// where its users should go. Each node announces its own copy; clients
/// act on it once a threshold of nodes announce byte-equal values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Encodable, Decodable, JsonSchema)]
pub struct ExpiryStatus {
    /// The expiry date as a unix timestamp in seconds; by convention
    /// midnight UTC of that day
    pub timestamp: u64,
    /// Invite code of the successor mint for users to migrate their funds
    /// to; absent when there is none
    pub successor: Option<InviteCode>,
}
