//! A single kept-alive, self-reconnecting iroh connection, published as a
//! `watch<Option<ConnState>>`. Both the mint node pool ([`crate::api`])
//! and the client's gateway connection pool are just this primitive
//! mapped over a set of iroh public keys — the mint client over its fixed
//! node set, the gateway pool over an append-only set of announced
//! gateways.

use std::time::Duration;

use anyhow::anyhow;
use iroh::endpoint::{Connection, PathId};
use iroh::{Endpoint, PublicKey};
use picomint_core::backoff::{BackoffBuilder, networking_backoff};

use crate::{ALPN, request_on_connection};
use picomint_encoding::{Decodable, Encodable};
use tokio::sync::watch;
use tokio::time::sleep;
use tracing::{debug, warn};

/// Live connection state for one node, published on a watch channel by its
/// [`connection_task`]. `None` (the channel's initial value) means the task
/// has started but not yet produced a first result.
#[derive(Debug, Clone)]
pub enum ConnState {
    Connected(Connection),
    Disconnected,
}

/// Public, connection-free view of a node's reachability for status streams.
/// Mirrors [`ConnState`] but carries the round-trip-time estimate in place of
/// the live connection handle, so it can cross the public client API boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnStatus {
    Connected(Duration),
    Disconnected,
}

impl ConnState {
    /// Snapshot this state as a [`ConnStatus`], reading the current RTT
    /// estimate off the live connection. Sampled at call time, so it reflects
    /// the RTT whenever the owning watch channel last fired (i.e. at connect).
    pub fn status(&self) -> ConnStatus {
        match self {
            ConnState::Connected(conn) => {
                ConnStatus::Connected(conn.rtt(PathId::ZERO).unwrap_or_default())
            }
            ConnState::Disconnected => ConnStatus::Disconnected,
        }
    }
}

/// Keep one iroh connection to `iroh_pk` alive, publishing each transition on
/// `state`. Connect, announce `Connected`, block on `Connection::closed`,
/// announce `Disconnected`, then reconnect. Connect failures back off via
/// `networking_backoff`, reset on success.
///
/// Returns once every receiver has been dropped, so a pool that goes out of
/// scope takes its tasks with it. Without that a short-lived pool would leak
/// one reconnecting task per node for the life of the process.
pub async fn connection_task(
    iroh_pk: PublicKey,
    endpoint: Endpoint,
    state: watch::Sender<Option<ConnState>>,
) {
    let reconnect = async {
        let mut backoff = networking_backoff().build();

        loop {
            match endpoint.connect(iroh_pk, ALPN).await {
                Ok(conn) => {
                    backoff = networking_backoff().build();

                    state.send_replace(Some(ConnState::Connected(conn.clone())));

                    conn.closed().await;

                    state.send_replace(Some(ConnState::Disconnected));
                }
                Err(error) => {
                    warn!(%iroh_pk, error = %format_args!("{error:#}"), "connect failed");

                    // Publish the failure: a request waiting for the first
                    // state must error out, not hang until the peer appears.
                    state.send_replace(Some(ConnState::Disconnected));

                    sleep(backoff.next().expect("networking_backoff retries forever")).await;
                }
            }
        }
    };

    tokio::select! {
        () = state.closed() => {}
        () = reconnect => {}
    }
}

/// Wait for `rx` to report its first state, then send `method` over the pooled
/// connection by opening a fresh bi stream. Errors if the current state is
/// `Disconnected`, or if the [`connection_task`] has gone (its `watch::Sender`
/// dropped — e.g. a gateway dropped from the announced set aborts its task);
/// either way the caller's retry layer reissues the request.
pub async fn request_on_state<R: Decodable>(
    rx: &mut watch::Receiver<Option<ConnState>>,
    method: impl Encodable,
) -> anyhow::Result<R> {
    let state = rx
        .wait_for(Option::is_some)
        .await
        .map_err(|_| anyhow!("Connection task is gone"))?
        .clone()
        .expect("wait_for guarantees Some");

    let ConnState::Connected(conn) = state else {
        return Err(anyhow!("Not connected"));
    };

    request_on_connection(&conn, method).await
}

/// As [`request_on_state`] but never gives up: the request is reissued on
/// every transition of the connection state and otherwise sleeps on the
/// watch channel. A lost connection is therefore awaited, not polled, and
/// the reconnect backoff in [`connection_task`] is the only backoff that
/// bounds how long a request waits. An error on a live connection is not a
/// transport fault — a server-side error or a decode mismatch — so the node
/// counts as unresponsive until its connection flips. Errors only if the
/// [`connection_task`] is gone, which no reconnect can cure.
pub async fn request_on_state_retry<R: Decodable>(
    rx: &mut watch::Receiver<Option<ConnState>>,
    method: impl Encodable + Clone,
) -> anyhow::Result<R> {
    loop {
        let state = rx
            .wait_for(Option::is_some)
            .await
            .map_err(|_| anyhow!("Connection task is gone"))?
            .clone()
            .expect("wait_for guarantees Some");

        if let ConnState::Connected(conn) = state {
            match request_on_connection(&conn, method.clone()).await {
                Ok(response) => return Ok(response),
                Err(e) => debug!(error = %e, "Node request failed"),
            }
        }

        rx.changed()
            .await
            .map_err(|_| anyhow!("Connection task is gone"))?;
    }
}
