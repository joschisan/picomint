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

pub use add_mint::AddMintError;
pub use client::{Client, NotAddedError};
pub use picomint_core::core::{Account, OperationId};
pub use picomint_rpc::connection::ConnStatus;
pub use secret::{Mnemonic, random as random_mnemonic};

use crate::eventlog::{Event, EventKind};
use picomint_core::sql::SqlRow;
use picomint_core::{Amount, TransactionId};
use serde::{Deserialize, Serialize};

/// The client submitted a transaction to the mint; `tx_accept` or
/// `tx_reject` follows under the same operation.
#[derive(Serialize, Deserialize, Debug, Clone, SqlRow)]
pub struct TxCreateEvent {
    /// The transaction id, hex
    pub txid: TransactionId,
    /// What the notes spent exceeded the amount needed by, in msat: change
    /// the mint reissues to the account as fresh notes once accepted
    pub reissue: Amount,
    /// The mint's fee on the transaction, in msat: its per-input and
    /// per-output fees summed
    pub fee: Amount,
}

impl Event for TxCreateEvent {
    const KIND: EventKind = EventKind::from_static("tx-create");
}

/// The mint accepted the transaction into consensus; `ts - tx_create.ts`
/// is the acceptance latency.
#[derive(Serialize, Deserialize, Debug, Clone, SqlRow)]
pub struct TxAcceptEvent {
    /// The transaction id, hex
    pub txid: TransactionId,
}

impl Event for TxAcceptEvent {
    const KIND: EventKind = EventKind::from_static("tx-accept");
}

/// The mint rejected the transaction; nothing was spent.
#[derive(Serialize, Deserialize, Debug, Clone, SqlRow)]
pub struct TxRejectEvent {
    /// The transaction id, hex
    pub txid: TransactionId,
    /// The mint's reason
    pub error: String,
}
impl Event for TxRejectEvent {
    const KIND: EventKind = EventKind::from_static("tx-reject");
}
