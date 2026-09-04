//! Picomint client library.
//!
//! [`Client`] is the entry point for applications: one instance per app,
//! holding every added mint as data. [`Client::new`] brings every
//! added mint up; [`Client::add_mint`] adds one, [`Client::begin_remove_mint`]
//! wipes one. An added mint is always up — there is no dormant state
//! in between. Every operation takes the
//! [`picomint_core::config::MintId`] it acts on
//! and is named for the module that serves it — `ecash_send`,
//! `onchain_receive`, `lightning_receive`, `gateway_finalize_send` — so there is
//! no per-mint handle to hold or leak.
//!
//! Every table is shared across mints with a
//! [`picomint_core::config::MintId`]-prefixed key, so adds, removes,
//! and all module writes commit through one database.
//!
//! Per-module logic lives in [`mod@ecash`], [`mod@onchain`], [`mod@lightning`], and
//! [`mod@gateway`]. Each module owns its own state machines and contributes its
//! slice of the flat [`Client`] surface. Submission ownership lives
//! entirely in the ecash module — non-ecash modules build a
//! [`crate::tx::TxBuilder`] and call its `finalize_and_submit_tx`, which
//! balances against the wallet and submits via its own
//! [`crate::tx::TxSubmissionStateMachine`].

/// Downloading a mint's config and rebuilding what the seed owns there.
mod add_mint;
/// Mint API transport
pub mod api;

/// Core [`Client`]
mod client;
/// The per-mint [`context::ClientContext`]
mod context;
/// Ecash module client.
pub mod ecash;
/// Append-only event log shared by all mints on this host.
pub mod eventlog;
/// Per-module typed state machine executor
mod executor;
/// Mint expiry-status cache + refresh.
pub mod expiry;
/// Gateway lightning module (mounted by the gateway daemon).
pub mod gateway;
/// Lightning module client.
pub mod lightning;
/// Onchain module client.
pub mod onchain;
/// Secret handling & derivation
pub mod secret;
/// Local `(TaskTracker, CancellationToken)` wrapper for client background tasks.
mod task;
/// Structs and interfaces to construct Picomint transactions
pub mod tx;

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
    /// wallet (minus mint fees on the change outputs) as fresh
    /// notes once the tx is accepted.
    pub remint: Amount,
    /// Mint fee paid by this transaction (sum of per-input and
    /// per-output fees the mint deducts).
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
