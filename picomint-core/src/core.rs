//! Picomint Core API (common) module interface
//!
//! This module defines common interoperability types
//! and functionality that is used on both client and sever side.

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
