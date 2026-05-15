//! Picomint Core library
//!
//! `picomint-core` contains commonly used types, utilities and primitives,
//! shared between both client and guardian code.
//!
//! Things that are guardian-side only typically live in `picomint-guardian-daemon`,
//! and client-side only in `picomint-client`.

extern crate self as picomint_core;

pub use amount::*;
/// Mostly re-exported for [`Decodable`] macros.
pub use anyhow;
pub use bitcoin::hashes::Hash as BitcoinHash;
pub use peer::*;
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
/// Guardian-announced federation expiry date.
pub mod expiry;
/// Federation invite code
pub mod invite;
/// Lightning module wire types / helpers (shared between client and server).
pub mod ln;
/// Guardian wire method names dispatched over Iroh.
pub mod methods;
/// Mint module wire types / helpers (shared between client and server).
pub mod mint;
/// Extendable module sysystem
pub mod module;
/// `PeerId` type
mod peer;
/// Consensus session outcome types (AcceptedItem, SessionOutcome, …).
pub mod secret;

pub mod session;
/// Wire-level Transaction and ConsensusItem types.
pub mod tx;
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
