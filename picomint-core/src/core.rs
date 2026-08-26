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

/// One of a client's balances within a single federation.
///
/// The federation cannot tell accounts apart — an account is purely a
/// client-side split of the derivation tree and of the few tables that hold
/// per-account state. It lives here rather than in the client because the
/// event log is tagged with it, and the log is written by a crate that cannot
/// depend on the client.
///
/// Used as the leading component of every account-scoped table key and as a
/// hop in the derivation tree, so variant order is load-bearing — reordering
/// silently re-keys every client.
///
/// The user's balances are one nested variant rather than a variant each, so
/// that they and the accounts the client keeps for itself grow in separate
/// namespaces. A user balance added later is appended inside
/// [`UserAccount`], which leaves every path outside that subtree — including
/// [`Account::IntegratorFee`]'s — exactly where it was.
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
    /// A balance the user spends and is shown.
    User(UserAccount),
    /// Where the federation's per-transaction cut accrues, if its guardians
    /// announced one.
    ///
    /// Not a destination a counterparty can pay into either: like
    /// [`Account::IntegratorFee`] it is funded only by outputs the client
    /// adds to its own transactions, and drained only by paying them out.
    /// The two differ in who decides the cut — guardians by announcement,
    /// the integrator by argument — and in whose lnurl it leaves to.
    OperatorFee,
    /// Where an integrator's per-transaction cut accrues, if it configured
    /// one. Never a destination a counterparty can pay into: it is funded
    /// only by outputs the client adds to its own transactions, and drained
    /// only by the integrator spending from it.
    IntegratorFee,
}

picomint_redb::consensus_key!(Account);

/// One of the balances belonging to the user of a client, as opposed to one
/// the client keeps for its integrator.
///
/// Its own enum so that the set can grow without disturbing anything: a
/// variant appended here extends [`Account::User`]'s subtree of the
/// derivation tree and leaves the rest of it untouched.
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
pub enum UserAccount {
    Primary,
    Secondary,
    Tertiary,
}

impl From<UserAccount> for Account {
    fn from(account: UserAccount) -> Self {
        Account::User(account)
    }
}

impl Account {
    /// Spelt out rather than written [`Account::User`] of each, since these
    /// are what a caller names when it means one particular balance and
    /// nothing about the enclosing enum is what it is saying.
    pub const PRIMARY: Account = Account::User(UserAccount::Primary);
    pub const SECONDARY: Account = Account::User(UserAccount::Secondary);
    pub const TERTIARY: Account = Account::User(UserAccount::Tertiary);

    /// Every account a counterparty can pay into, in key order — and so the
    /// ones the address and contract scanners trial their keys against.
    ///
    /// [`Account::IntegratorFee`] is reachable only from inside a transaction the
    /// client builds, so a scanner that swept for it would derive a key per
    /// entry of a federation-wide stream to match something that cannot be
    /// there.
    ///
    /// The set is fixed on purpose: every account exists from the moment a
    /// client is built, so no account ever joins a stream late.
    pub const USER_ACCOUNTS: [Account; 3] =
        [Account::PRIMARY, Account::SECONDARY, Account::TERTIARY];
}
