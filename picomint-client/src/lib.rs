//! Picomint client library.
//!
//! [`Client`] is the entry point for applications interacting with one
//! federation. Use [`Client::new`] for regular clients and
//! [`Client::new_gateway`] for the gateway daemon's flavor — both take a
//! [`ConsensusConfig`] that the integrator obtained via [`download`] (from
//! an invite code) and persists itself.
//!
//! Per-module logic lives in [`mod@mint`], [`mod@wallet`], [`mod@ln`], and
//! [`mod@gw`]. Each module owns its own state machines and exposes a
//! `*Module::new` constructor used by the [`Client`] entry points.
//! Submission ownership lives entirely in [`crate::mint::MintClientModule`]
//! — non-mint modules build a [`crate::tx::TxBuilder`]
//! and call `MintClientModule::finalize_and_submit_tx`, which
//! balances against the wallet and submits via its own
//! [`crate::tx::TxSubmissionStateMachine`].

/// Declare a per-federation table. Expands to a tuple struct
/// `Name(pub FederationId)` implementing [`picomint_redb::Table`] with
/// resolved name `"{federation}/{label}"`. Multiple federation clients sharing
/// one root [`picomint_redb::Database`] (as in the gateway daemon) get
/// disjoint on-disk keyspaces this way.
#[macro_export]
macro_rules! client_table {
    (
        $(#[$attr:meta])*
        $name:ident,
        $k:ty => $v:ty,
        $label:literal $(,)?
    ) => {
        $(#[$attr])*
        #[derive(Copy, Clone, Debug)]
        pub struct $name(pub ::picomint_core::config::FederationId);

        impl ::picomint_redb::Table for $name {
            type Key = $k;
            type Value = $v;

            fn resolved_name(&self) -> ::std::string::String {
                format!("{}/{}", self.0, $label)
            }
        }
    };
}

/// Federation API transport
pub mod api;
/// Core [`Client`]
mod client;
/// Shared kept-alive iroh connection primitive (federation peers + gateways).
mod connection;
/// Per-module typed state machine executor
pub mod executor;
/// Federation expiry-status cache + refresh.
pub mod expiry;
/// Gateway lightning module (mounted by the gateway daemon).
pub mod gw;
/// Lightning module client.
pub mod ln;
/// Mint module client.
pub mod mint;
/// Module client interface definitions
pub mod module;
/// Client query-consensus strategies
pub mod query;
/// Secret handling & derivation
pub mod secret;
/// Local `(TaskTracker, CancellationToken)` wrapper for client background tasks.
mod task;
/// Structs and interfaces to construct Picomint transactions
pub mod tx;
/// Wallet module client.
pub mod wallet;

use anyhow::bail;
pub use iroh::Endpoint;
use picomint_core::config::ConsensusConfig;
use picomint_core::invite::InviteCode;
use picomint_core::methods::{ConfigRequest, ConfigResponse, CoreMethod};
use picomint_core::module::Method;
use tracing::debug;

pub use client::Client;
pub use connection::ConnStatus;
pub use mint::commit_restore;
pub use picomint_core::core::{Account, OperationId};
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
    /// every other per-output fee, so the two do not overlap and the value
    /// here is what actually reached the account.
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

/// Downloads the [`ConsensusConfig`] from the issuing guardian named in the
/// invite code. The guardian enforces the invite's expiration and user limit
/// before serving; integrity is guaranteed because the config's computed
/// federation id must match the one committed in the invite code.
pub async fn download(endpoint: &Endpoint, invite: &InviteCode) -> anyhow::Result<ConsensusConfig> {
    debug!(
        invite = %picomint_base32::encode(invite),
        node_id = %invite.node_id,
        "Downloading client config via invite code"
    );

    let invite_resp: ConfigResponse = picomint_rpc::request(
        endpoint,
        invite.node_id,
        Method::Core(CoreMethod::Config(ConfigRequest {
            invite_id: invite.invite_id,
        })),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Failed to download client config from invite peer"))?;

    if invite_resp.config.calculate_federation_id() != invite.federation {
        bail!("FederationId in invite code does not match client config");
    }

    Ok(invite_resp.config)
}

/// Rebuild one account's notes from the seed by scanning the federation.
///
/// Reads nothing and writes nothing locally. Applying the result is two
/// steps, in this order:
///
/// 1. [`commit_restore`] in whichever dbtx marks the federation as added —
///    this persists the counter mark, and must land before the account issues
///    anything.
/// 2. [`mint::MintClientModule::receive`] on [`mint::Restore::ecash`], once
///    the client is up, to reissue the restored notes under nonces the
///    federation cannot tie back to the scan.
///
/// The marks are safe to persist without the reissuance. The reissuance is
/// not a no-op if repeated: a bundle can be received once per federation, so
/// running it a second time fails with
/// [`mint::ReceiveECashError::AlreadyAttempted`].
///
/// Covers a single [`Account`]. Restoring a whole client means running this
/// once per account and applying each result under the account it was asked
/// for — the counter marks can all be committed in one dbtx.
pub async fn restore(
    endpoint: &Endpoint,
    mnemonic: &Mnemonic,
    config: &ConsensusConfig,
    account: Account,
) -> anyhow::Result<mint::Restore> {
    let federation = config.calculate_federation_id();

    let peer_node_ids = config
        .peers
        .iter()
        .map(|(peer, endpoint)| (*peer, endpoint.iroh_pk))
        .collect();

    let api = api::FederationApi::new(endpoint.clone(), peer_node_ids);

    let secret = secret::ClientSecret::new(mnemonic, federation).mint_secret();

    mint::scan(&api, &secret, &config.mint, federation, account).await
}
