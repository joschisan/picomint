//! Picomint Core API (common) module interface
//!
//! This module defines common interoperability types
//! and functionality that is used on both client and sever side.
use std::fmt;

use bitcoin::hashes::sha256;
use derive_more::Display;
use picomint_encoding::{Decodable, Encodable};
use serde::{Deserialize, Serialize};

/// Unique identifier for one semantic, correlatable operation.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Encodable,
    Decodable,
    PartialOrd,
    Ord,
    Display,
)]
pub struct OperationId(pub sha256::Hash);

impl OperationId {
    /// Generate random [`OperationId`]
    pub fn new_random() -> Self {
        Self::from_encodable(&rand::random::<[u8; 32]>())
    }

    pub fn from_encodable<E: Encodable>(encodable: &E) -> Self {
        Self(encodable.consensus_hash::<sha256::Hash>())
    }
}

picomint_redb::consensus_key!(OperationId);

/// Type of a module in the fixed set.
///
/// Discriminants are also used as the `ChildId` input for per-module secret
/// derivation and as the stable wire tag. Order matters — do not reorder.
#[derive(
    Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Encodable, Decodable,
)]
pub enum ModuleKind {
    Mint,
    Ln,
    Wallet,
}

impl ModuleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mint => "mint",
            Self::Ln => "ln",
            Self::Wallet => "wallet",
        }
    }
}

impl fmt::Display for ModuleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for ModuleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
