//! Picomint Core library
//!
//! `picomint-core` contains commonly used types, utilities and primitives,
//! shared between both client and server code.
//!
//! Things that are server-side only typically live in `picomint-server`, and
//! client-side only in `picomint-client`.
//!
//! ### Wasm support
//!
//! All code in `picomint-core` needs to compile on Wasm, and `picomint-core`
//! includes helpers and wrappers around non-wasm-safe utitlies.
//!
//! In particular:
//!
//! * [`picomint_core::task`] for task spawning and control
//! * [`picomint_core::time`] for time-related operations

extern crate self as picomint_core;

pub use amount::*;
/// Mostly re-exported for [`Decodable`] macros.
pub use anyhow;
pub use bitcoin::hashes::Hash as BitcoinHash;
pub use peer_id::*;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
pub use {bitcoin, hex, secp256k1};

use picomint_encoding::{Decodable, Encodable};

/// Bitcoin amount types
mod amount;
/// Fibonacci backoff policies for retry loops.
pub mod backoff;
/// Federation configuration
pub mod config;
/// Fundamental types
pub mod core;
pub mod endpoint_constants;
/// Common environment variables
pub mod envs;
/// Federation invite code
pub mod invite_code;
/// Lightning module wire types / helpers (shared between client and server).
pub mod ln;
/// Mint module wire types / helpers (shared between client and server).
pub mod mint;
/// Extendable module sysystem
pub mod module;
/// `PeerId` type
mod peer_id;
/// Consensus session outcome types (AcceptedItem, SessionOutcome, …).
pub mod secret;

pub mod session_outcome;
/// Task handling, including wasm safe logic
pub mod task;
/// Time handling, wasm safe functionality
pub mod time;
/// Wire-level Transaction and ConsensusItem types.
pub mod transaction;
/// General purpose utilities
pub mod util;
/// Wallet module wire types / helpers (shared between client and server).
pub mod wallet;
/// Static wire enums over the fixed module set.
pub mod wire;

/// A transaction id for peg-ins, peg-outs and reissuances.
#[derive(
    Debug,
    Clone,
    Copy,
    Eq,
    PartialEq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    Encodable,
    Decodable,
    derive_more::Display,
)]
pub struct TransactionId(pub bitcoin::hashes::sha256::Hash);

picomint_redb::consensus_key!(TransactionId);

/// `InPoint` represents a globally unique input in a transaction
///
/// Hence, a transaction ID and the input index is required.
#[derive(
    Debug,
    Clone,
    Copy,
    Eq,
    PartialEq,
    PartialOrd,
    Ord,
    Hash,
    Deserialize,
    Serialize,
    Encodable,
    Decodable,
)]
pub struct InPoint {
    /// The referenced transaction ID
    pub txid: TransactionId,
    /// As a transaction may have multiple inputs, this refers to the index of
    /// the input in a transaction
    pub in_idx: u64,
}

impl std::fmt::Display for InPoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.txid, self.in_idx)
    }
}

/// `OutPoint` represents a globally unique output in a transaction
///
/// Hence, a transaction ID and the output index is required.
#[derive(
    Debug,
    Clone,
    Copy,
    Eq,
    PartialEq,
    PartialOrd,
    Ord,
    Hash,
    Deserialize,
    Serialize,
    Encodable,
    Decodable,
)]
pub struct OutPoint {
    /// The referenced transaction ID
    pub txid: TransactionId,
    /// As a transaction may have multiple outputs, this refers to the index of
    /// the output in a transaction
    pub out_idx: u64,
}

impl std::fmt::Display for OutPoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.txid, self.out_idx)
    }
}

picomint_redb::consensus_key!(OutPoint);
