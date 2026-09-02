//! Picomint client library.
//!
//! [`Client`] is the entry point for applications: one instance per app,
//! holding every joined federation as data. [`Client::new`] binds the iroh
//! endpoint from the seed; [`Client::add`] joins a federation,
//! [`Client::connect`] brings one up, [`Client::remove`] wipes one. Every
//! operation takes the [`picomint_core::config::FederationId`] it acts on
//! and is named for the module that serves it — `mint_send`,
//! `wallet_deposit_address`, `ln_receive`, `gw_finalize_send` — so there is
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

/// Federation API transport
/// Core [`Client`]
pub mod api;

mod client;
/// The per-federation [`context::ClientContext`]
mod context;
/// Shared kept-alive iroh connection primitive (federation peers + gateways).
/// Per-module typed state machine executor
mod executor;
/// Federation expiry-status cache + refresh.
pub mod expiry;
/// Federation fee announcement cache, and paying out a collected cut.
mod fee;
/// Gateway lightning module (mounted by the gateway daemon).
pub mod gw;
/// Downloading a federation's config and rebuilding what the seed owns there.
mod join;
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

use picomint_core::{Amount, TransactionId};
use picomint_eventlog::{Event, EventKind, EventSource};
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
    pub tx_fee: Amount,
    /// Integrator's cut, issued into [`Account::AppFee`] by this
    /// transaction. Zero for a client built with no cut, and for the
    /// collections that spend the account.
    ///
    /// What the cut cost the federation to issue is in [`Self::tx_fee`] with
    /// every other per-output fee, so the two do not overlap and this is what
    /// actually reached the account.
    pub app_fee: Amount,
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
