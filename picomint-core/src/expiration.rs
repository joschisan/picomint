//! Federation-wide expiration announcement. Guardians can collectively
//! declare a future shutdown date (and optionally a successor federation's
//! invite code) that clients fetch via threshold consensus and surface to
//! their users.

use picomint_encoding::{Decodable, Encodable};
use serde::{Deserialize, Serialize};

use crate::invite::InviteCode;

/// Status indicating that a federation is expiring, with a target date and
/// optional successor federation invite code for users to migrate to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Encodable, Decodable)]
pub struct ExpirationStatus {
    /// Expiration date as a unix timestamp (midnight UTC).
    pub date: u64,
    /// Optional invite code for the successor federation.
    pub successor: Option<InviteCode>,
}

picomint_redb::consensus_value!(ExpirationStatus);
