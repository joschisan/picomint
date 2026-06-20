//! Client-side gateway pool: each announced gateway's kept-alive iroh
//! connection and its latest probed info, managed together.
//!
//! Gateways are discovered dynamically via the federation's announced pk set,
//! so [`Gateways`] keys one entry per gateway holding both its pooled
//! connection (a [`connection_task`] published on a `watch`) and its latest
//! [`GatewayInfo`]. The two share one lifecycle: [`Gateways::reconcile`] spawns
//! an entry when a gateway joins the announced set and drops it — aborting the
//! connection task — when it leaves, so [`Gateways::select`] never returns a
//! gateway the federation no longer recognises. Surviving gateways keep their
//! warm connection across refreshes; the QUIC handshake and hole-punched path
//! are paid once, then reused by info probes, sends, and receives.
//!
//! The wire types ([`GatewayMethod`] + per-method `*Request`/`*Response`
//! structs) live in [`picomint_core::ln::methods`] because the gateway daemon
//! must agree on them. The wire envelope is `Result<Vec<u8>, String>` — same
//! shape as the federation API.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use anyhow::Context;
use bitcoin::secp256k1::schnorr::Signature;
use iroh::Endpoint;
use lightning_invoice::Bolt11Invoice;
use picomint_core::OutPoint;
use picomint_core::config::FederationId;
use picomint_core::ln::LightningInvoice;
use picomint_core::ln::contracts::{IncomingContract, OutgoingContract};
use picomint_core::ln::gateway::{GatewayInfo, GatewayPk};
use picomint_core::ln::methods::{
    GatewayMethod, InfoRequest, InfoResponse, ReceiveRequest, ReceiveResponse, SendRequest,
    SendResponse,
};
use picomint_encoding::Decodable;
use rand::seq::IteratorRandom;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_util::task::AbortOnDropHandle;

use crate::connection::{ConnState, connection_task, request_on_state};

/// One announced gateway: its pooled connection and the latest info probe,
/// dropped together when the gateway leaves the announced set.
struct Gateway {
    /// `None` until the first successful info probe. A gateway without info is
    /// not selectable but keeps its warm connection.
    info: Option<GatewayInfo>,
    conn: watch::Receiver<Option<ConnState>>,
    /// Aborts the gateway's [`connection_task`] when this entry is dropped.
    _task: AbortOnDropHandle<()>,
}

/// Pool of announced gateways keyed by node id, each with a kept-alive
/// connection and its latest info. Cloneable; one instance is shared by the
/// lightning module and its send state machine.
#[derive(Clone)]
pub struct Gateways {
    endpoint: Endpoint,
    inner: Arc<RwLock<BTreeMap<GatewayPk, Gateway>>>,
}

impl Gateways {
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            inner: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    /// Bring the connection pool in line with the announced gateway set `pks`:
    /// spawn a kept-alive [`connection_task`] for each pk not already pooled,
    /// leaving surviving gateways' warm connections untouched.
    ///
    /// `prune` distinguishes the two callers. The authoritative
    /// `update_gateway_pks` passes `true`: gateways no longer announced are
    /// dropped (their connection task aborted). The cold-start
    /// `update_gateway_info` passes `false`: it only adds connections for the
    /// previous session's persisted pks, so it can run concurrently with the
    /// authoritative refresh without racing it for membership.
    pub fn reconcile(&self, pks: &[GatewayPk], prune: bool) {
        let mut map = self.inner.write().expect("gateways RwLock poisoned");

        if prune {
            map.retain(|pk, _| pks.contains(pk));
        }

        for pk in pks {
            map.entry(*pk).or_insert_with(|| {
                let (tx, rx) = watch::channel(None);
                let task = tokio::spawn(connection_task(pk.0, self.endpoint.clone(), tx));
                Gateway {
                    info: None,
                    conn: rx,
                    _task: AbortOnDropHandle::new(task),
                }
            });
        }
    }

    /// Probe `info` for each gateway in `pks` concurrently over its pooled
    /// connection, writing each result back as it arrives. A failed probe
    /// clears that gateway's info (unselectable) without dropping its
    /// connection; a slow probe never blocks the others' updates.
    pub async fn probe(&self, pks: &[GatewayPk], federation: FederationId) {
        let mut probes: JoinSet<(GatewayPk, Option<GatewayInfo>)> = JoinSet::new();

        for pk in pks {
            let this = self.clone();
            let pk = *pk;
            probes.spawn(async move {
                let info = this.gateway_info(pk, federation).await.ok().flatten();
                (pk, info)
            });
        }

        while let Some(Ok((pk, info))) = probes.join_next().await {
            if let Some(gateway) = self
                .inner
                .write()
                .expect("gateways RwLock poisoned")
                .get_mut(&pk)
            {
                gateway.info = info;
            }
        }
    }

    /// Pick a member gateway that has info. With `invoice = Some`, prefer a
    /// direct-swap gateway whose lightning pk matches the invoice's recovered
    /// payee (an ecash swap, no LN routing); otherwise pick at random for load
    /// distribution. Returns `None` if no gateway currently has info.
    pub fn select(&self, invoice: Option<&Bolt11Invoice>) -> Option<(GatewayPk, GatewayInfo)> {
        let map = self.inner.read().expect("gateways RwLock poisoned");

        if let Some(invoice) = invoice {
            let payee = invoice.recover_payee_pub_key();

            for (pk, gateway) in map.iter() {
                if let Some(info) = &gateway.info
                    && info.lightning_public_key == payee
                {
                    return Some((*pk, info.clone()));
                }
            }
        }

        map.iter()
            .filter_map(|(pk, gateway)| gateway.info.clone().map(|info| (*pk, info)))
            .choose(&mut rand::thread_rng())
    }

    /// Status watch for `gateway_pk`, if it is a current member.
    fn connection(&self, gateway_pk: GatewayPk) -> Option<watch::Receiver<Option<ConnState>>> {
        self.inner
            .read()
            .expect("gateways RwLock poisoned")
            .get(&gateway_pk)
            .map(|gateway| gateway.conn.clone())
    }

    async fn request<R: Decodable>(
        &self,
        gateway_pk: GatewayPk,
        method: GatewayMethod,
    ) -> anyhow::Result<R> {
        let mut rx = self
            .connection(gateway_pk)
            .context("Gateway is not a current member")?;

        request_on_state(&mut rx, method).await
    }

    pub async fn gateway_info(
        &self,
        gateway_pk: GatewayPk,
        federation: FederationId,
    ) -> anyhow::Result<Option<GatewayInfo>> {
        self.request::<InfoResponse>(gateway_pk, GatewayMethod::Info(InfoRequest { federation }))
            .await
            .map(|r| r.info)
    }

    pub async fn receive(
        &self,
        gateway_pk: GatewayPk,
        federation: FederationId,
        contract: IncomingContract,
    ) -> anyhow::Result<Bolt11Invoice> {
        self.request::<ReceiveResponse>(
            gateway_pk,
            GatewayMethod::Receive(ReceiveRequest {
                federation,
                contract,
            }),
        )
        .await
        .map(|r| r.invoice)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn send(
        &self,
        gateway_pk: GatewayPk,
        federation: FederationId,
        outpoint: OutPoint,
        contract: OutgoingContract,
        invoice: LightningInvoice,
        auth: Signature,
    ) -> anyhow::Result<Result<[u8; 32], Signature>> {
        self.request::<SendResponse>(
            gateway_pk,
            GatewayMethod::Send(SendRequest {
                federation,
                outpoint,
                contract,
                invoice,
                auth,
            }),
        )
        .await
        .map(|r| r.result)
    }
}
