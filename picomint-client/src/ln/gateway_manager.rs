//! Manages live iroh connections to every gateway registered with the
//! federation. Mirrors the federation-peer reconnect loop in
//! `crate::api::mod::connection_task` — one task per gateway,
//! reconnecting with `networking_backoff`, state published via a
//! `watch::channel`.
//!
//! Lifecycle:
//! 1. `spawn` returns immediately. An `init_task` retries
//!    `ln_gateways()` with `networking_backoff` until it gets a
//!    non-empty list, then constructs the inner state (one per-gateway
//!    task per node-id) and publishes it. The init task runs once —
//!    the gateway set is never refreshed afterwards.
//! 2. Each per-gateway task dials its gateway with `PICOMINT_ALPN`,
//!    fetches `GatewayInfo`, publishes `Some(Online { .. })`, then
//!    awaits `closed()` and publishes `Some(Offline)` before backing
//!    off and reconnecting.
//! 3. Callers consume state via `snapshot` (non-blocking) or
//!    `wait_any_first_attempt` (blocks until init completes AND at
//!    least one per-gateway task has reported its first dial outcome).

use std::collections::BTreeMap;

use iroh::Endpoint;
use iroh::endpoint::Connection;
use picomint_core::backoff::{BackoffBuilder, networking_backoff};
use picomint_core::config::FederationId;
use picomint_core::ln::gateway_api::GatewayInfo;
use picomint_core::module::PICOMINT_ALPN;
use picomint_core::task::TaskGroup;
use picomint_logging::LOG_CLIENT_NET;
use tokio::sync::watch;
use tokio::time::sleep;
use tracing::debug;

use crate::api::FederationApi;
use crate::ln::gateway_api as gw_api;

/// Per-gateway state published on the watch channel. `None` = first
/// dial attempt is still in flight; `Some(Online/Offline)` = first
/// attempt has resolved, subsequent reconnects flip between the two.
#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
pub enum GatewayState {
    Online {
        connection: Connection,
        gateway_info: GatewayInfo,
    },
    Offline,
}

#[derive(Clone)]
struct InnerState {
    states: BTreeMap<iroh::PublicKey, watch::Receiver<Option<GatewayState>>>,
}

#[derive(Clone)]
pub struct LnGatewayManager {
    /// `None` until the init task has fetched a non-empty gateway set
    /// from the federation; `Some` afterwards. Once populated, the
    /// inner map is frozen — same set forever.
    state: watch::Receiver<Option<InnerState>>,
}

impl LnGatewayManager {
    /// Spawn the init task. Returns immediately — the federation fetch
    /// happens in the background and is published via the watch channel
    /// when it resolves.
    pub fn spawn(
        endpoint: Endpoint,
        federation_id: FederationId,
        api: FederationApi,
        task_group: &TaskGroup,
    ) -> Self {
        let (tx, rx) = watch::channel(None);
        let task_group_clone = task_group.clone();
        task_group.spawn_cancellable("ln_gateway_manager_init", async move {
            init_task(endpoint, federation_id, api, task_group_clone, tx).await;
        });
        Self { state: rx }
    }

    /// Block until the init task has populated the gateway set AND at
    /// least one per-gateway task has reported its first dial outcome.
    /// Used by send/receive entry points so they don't race ahead of
    /// the manager.
    pub async fn wait_any_first_attempt(&self) {
        let mut rx = self.state.clone();
        let inner = match rx.wait_for(|s| s.is_some()).await {
            Ok(s) => s.clone().expect("wait_for guaranteed Some"),
            Err(_) => return,
        };
        let mut rxs: Vec<_> = inner.states.values().cloned().collect();
        for rx in rxs.iter_mut() {
            rx.wait_for(|s| s.is_some()).await.ok();
        }
    }

    pub fn known_gateways(&self) -> Vec<iroh::PublicKey> {
        self.state
            .borrow()
            .as_ref()
            .map(|inner| inner.states.keys().copied().collect())
            .unwrap_or_default()
    }

    /// Non-blocking snapshot of the current state for `node_id`.
    /// `None` means we don't track this gateway (or init hasn't
    /// completed yet). `Some(None)` means the per-gateway task hasn't
    /// completed its first dial attempt yet.
    pub fn snapshot(&self, node_id: &iroh::PublicKey) -> Option<Option<GatewayState>> {
        self.state
            .borrow()
            .as_ref()
            .and_then(|inner| inner.states.get(node_id).map(|rx| rx.borrow().clone()))
    }

    /// Block until this gateway's task has recorded its first dial
    /// outcome. Returns `None` if the gateway is unknown after init.
    /// If init hasn't completed yet, this waits for it first.
    pub async fn wait_first_attempt(&self, node_id: &iroh::PublicKey) -> Option<GatewayState> {
        let mut state_rx = self.state.clone();
        let inner = state_rx.wait_for(|s| s.is_some()).await.ok()?.clone()?;
        let mut rx = inner.states.get(node_id)?.clone();
        let value = rx
            .wait_for(|s| s.is_some())
            .await
            .ok()?
            .clone()
            .expect("wait_for guaranteed Some");
        Some(value)
    }

    /// Return the first gateway whose current state is `Online`. Used
    /// by the receive path and by sends where the invoice's payee
    /// doesn't match any known gateway. Non-blocking: callers should
    /// `wait_any_first_attempt` first if they need the manager to be
    /// initialized.
    pub fn any_online(&self) -> Option<(iroh::PublicKey, GatewayState)> {
        let borrow = self.state.borrow();
        let inner = borrow.as_ref()?;
        for (node_id, rx) in inner.states.iter() {
            if let Some(state @ GatewayState::Online { .. }) = rx.borrow().clone() {
                return Some((*node_id, state));
            }
        }
        None
    }
}

/// Retries `ln_gateways()` with `networking_backoff` until it succeeds
/// (an empty list is treated as success — callers see no online
/// gateways and decide what to do), then spawns one `gateway_task`
/// per node-id and publishes the resulting state. Runs once — the
/// manager is never refreshed afterwards.
async fn init_task(
    endpoint: Endpoint,
    federation_id: FederationId,
    api: FederationApi,
    task_group: TaskGroup,
    tx: watch::Sender<Option<InnerState>>,
) {
    let mut backoff = networking_backoff().build();

    let gateways = loop {
        match api.ln_gateways().await {
            Ok(gateways) => break gateways,
            Err(e) => {
                debug!(
                    target: LOG_CLIENT_NET,
                    err = ?e,
                    "ln_gateways fetch failed, backing off",
                );
                sleep(backoff.next().expect("networking_backoff retries forever")).await;
            }
        }
    };

    let mut states = BTreeMap::new();
    for node_id in gateways {
        let (gw_tx, gw_rx) = watch::channel(None);
        let endpoint = endpoint.clone();
        task_group.spawn_cancellable(
            format!("gateway-conn-{}", node_id.fmt_short()),
            async move {
                gateway_task(endpoint, federation_id, node_id, gw_tx).await;
            },
        );
        states.insert(node_id, gw_rx);
    }

    let _ = tx.send_replace(Some(InnerState { states }));
}

async fn gateway_task(
    endpoint: Endpoint,
    federation_id: FederationId,
    node_id: iroh::PublicKey,
    tx: watch::Sender<Option<GatewayState>>,
) {
    let mut backoff = networking_backoff().build();

    loop {
        match dial_and_handshake(&endpoint, &federation_id, &node_id).await {
            Ok((connection, gateway_info)) => {
                backoff = networking_backoff().build();
                let _ = tx.send_replace(Some(GatewayState::Online {
                    connection: connection.clone(),
                    gateway_info,
                }));
                connection.closed().await;
                let _ = tx.send_replace(Some(GatewayState::Offline));
            }
            Err(e) => {
                debug!(
                    target: LOG_CLIENT_NET,
                    %node_id,
                    err = %e,
                    "Gateway dial failed, backing off"
                );
                let _ = tx.send_replace(Some(GatewayState::Offline));
                sleep(backoff.next().expect("networking_backoff retries forever")).await;
            }
        }
    }
}

async fn dial_and_handshake(
    endpoint: &Endpoint,
    federation_id: &FederationId,
    node_id: &iroh::PublicKey,
) -> anyhow::Result<(Connection, GatewayInfo)> {
    let connection = endpoint
        .connect(*node_id, PICOMINT_ALPN)
        .await
        .map_err(|e| anyhow::anyhow!("connect: {e}"))?;

    let gateway_info = gw_api::gateway_info(&connection, federation_id)
        .await
        .map_err(|e| anyhow::anyhow!("gateway_info: {e}"))?;

    Ok((connection, gateway_info))
}
