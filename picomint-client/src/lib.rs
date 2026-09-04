//! Picomint client library.
//!
//! [`Client`] is the entry point for applications: one instance per app,
//! holding every added federation as data. [`Client::new`] brings every
//! added federation up; [`Client::add`] adds one, [`Client::begin_remove`]
//! wipes one. An added federation is always up — there is no dormant state
//! in between. Every operation takes the
//! [`picomint_core::config::FederationId`] it acts on
//! and is named for the module that serves it — `mint_send`,
//! `wallet_receive`, `ln_receive`, `gw_finalize_send` — so there is
//! no per-federation handle to hold or leak.
//!
//! Every table is shared across federations with a
//! [`picomint_core::config::FederationId`]-prefixed key, so adds, removes,
//! and all module writes commit through one database.
//!
//! Per-module logic lives in [`mod@mint`], [`mod@wallet`], [`mod@ln`], and
//! [`mod@gw`]. Each module owns its own state machines and contributes its
//! slice of the flat [`Client`] surface. Submission ownership lives
//! entirely in the mint module — non-mint modules build a
//! [`crate::tx::TxBuilder`] and call its `finalize_and_submit_tx`, which
//! balances against the wallet and submits via its own
//! [`crate::tx::TxSubmissionStateMachine`].

/// Downloading a federation's config and rebuilding what the seed owns there.
mod add;
/// Federation API transport
/// Core [`Client`]
pub mod api;

mod client;
/// The per-federation [`context::ClientContext`]
mod context;
/// Append-only event log shared by all federations on this host.
pub mod eventlog;
/// Shared kept-alive iroh connection primitive (federation peers + gateways).
/// Per-module typed state machine executor
mod executor;
/// Federation expiry-status cache + refresh.
pub mod expiry;
/// Federation fee announcement cache, and paying out a collected cut.
/// Gateway lightning module (mounted by the gateway daemon).
pub mod gw;
/// Lightning module client.
pub mod ln;
/// Mint module client.
pub mod mint;
/// Client query-consensus strategies
/// Secret handling & derivation
pub mod secret;
/// Local `(TaskTracker, CancellationToken)` wrapper for client background tasks.
mod task;
/// Structs and interfaces to construct Picomint transactions
pub mod tx;
/// Wallet module client.
pub mod wallet;

pub use iroh::Endpoint;

pub use client::Client;
pub use picomint_core::core::{Account, OperationId};
pub use picomint_rpc::connection::ConnStatus;
pub use secret::{Mnemonic, random as random_mnemonic};

use crate::eventlog::{Event, EventKind, EventSource};
use picomint_core::{Amount, TransactionId};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TxCreateEvent {
    pub txid: TransactionId,
    /// Amount the mint over-funded by when balancing the caller's
    /// builder: `sum(funding_notes) - deficit`. Reissued back to the
    /// wallet (minus federation fees on the change outputs) as fresh
    /// notes once the tx is accepted.
    pub remint: Amount,
    /// Federation fee paid by this transaction (sum of per-input and
    /// per-output fees the federation deducts).
    pub fee: Amount,
}

impl Event for TxCreateEvent {
    const SOURCE: EventSource = EventSource::Core;
    const KIND: EventKind = EventKind::from_static("tx-create");
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TxAcceptEvent {
    pub txid: TransactionId,
}

impl Event for TxAcceptEvent {
    const SOURCE: EventSource = EventSource::Core;
    const KIND: EventKind = EventKind::from_static("tx-accept");
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TxRejectEvent {
    pub txid: TransactionId,
    pub error: String,
}
impl Event for TxRejectEvent {
    const SOURCE: EventSource = EventSource::Core;
    const KIND: EventKind = EventKind::from_static("tx-reject");
}
