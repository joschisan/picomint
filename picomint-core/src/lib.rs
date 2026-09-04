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
pub use node::*;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
pub use {bitcoin, hex, secp256k1};

use picomint_encoding::{Decodable, Encodable};

/// Bitcoin amount types
mod amount;
/// Fibonacci backoff policies for retry loops.
pub mod backoff;
/// Mint configuration
pub mod config;
/// Fundamental types
pub mod core;
/// Guardian-announced mint expiry date.
pub mod expiry;
/// A cut charged on a client's transactions, and where to pay it out.
/// Mint invite code
pub mod invite;
/// Lightning module wire types / helpers (shared between client and server).
pub mod lightning;
/// Guardian wire method names dispatched over Iroh.
pub mod methods;
/// ECash module wire types / helpers (shared between client and server).
pub mod ecash;
/// Extendable module sysystem
pub mod module;
/// `NodeId` type
mod node;
/// Consensus session outcome types (AcceptedItem, SessionOutcome, …).
pub mod secret;

pub mod session;
/// Wire-level Transaction and ConsensusItem types.
pub mod tx;
/// Consensus version of the mint and the vote that advances it.
pub mod version;
/// Onchain module wire types / helpers (shared between client and server).
pub mod onchain;
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
    /// the output in a transaction. A `u16` covers every index a valid
    /// transaction can have, since a transaction carries at most
    /// [`Transaction::MAX_OUTPUTS`] of them.
    ///
    /// [`Transaction::MAX_OUTPUTS`]: crate::tx::Transaction::MAX_OUTPUTS
    pub out_idx: u16,
}

impl std::fmt::Display for OutPoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.txid, self.out_idx)
    }
}
