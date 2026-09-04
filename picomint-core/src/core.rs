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

/// One of a client's balances within a single mint.
///
/// The mint cannot tell accounts apart — an account is purely a
/// client-side split of the derivation tree and of the few tables that hold
/// per-account state. It lives here rather than in the client because the
/// event log is tagged with it, and the log is written by a crate that cannot
/// depend on the client.
///
/// Used as the leading component of every account-scoped table key and as a
/// hop in the derivation tree, so variant order is load-bearing — reordering
/// silently re-keys every client.
///
/// Deliberately has no `Default` impl: every entry point that touches
/// account-scoped state takes one of these explicitly, so an omitted argument
/// is a compile error rather than a silent write to the wrong balance.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    Encodable,
    Decodable,
    Display,
)]
pub enum Account {
    Primary,
    Secondary,
    Tertiary,
    Quaternary,
    Quinary,
}

impl Account {
    /// Every account, in key order — the set a counterparty can pay into,
    /// the one the address and contract scanners trial their keys against,
    /// and the one a seed scan walks on restore.
    ///
    /// The set is fixed on purpose: every account exists from the moment a
    /// client is built, so no account ever joins a stream late.
    pub const ALL: [Account; 5] = [
        Account::Primary,
        Account::Secondary,
        Account::Tertiary,
        Account::Quaternary,
        Account::Quinary,
    ];
}
