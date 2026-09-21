//! Client-side pool of a mint's announced counterparties — the gateways of
//! the lightning module, the brokers of the swap module — each with its
//! kept-alive iroh connection and its latest probed info, managed together.
//!
//! Counterparties are discovered dynamically via the mint's announced pk
//! set, so a [`Pool`] keys one entry per pk holding both its pooled
//! connection (a [`connection_task`] published on a `watch`) and its latest
//! info. The two share one lifecycle: [`Pool::reconcile`] spawns an entry
//! when a pk joins the announced set and drops it — aborting the connection
//! task — when it leaves, so [`Pool::info`] never answers for a counterparty
//! the mint no longer recognises. Survivors keep their warm connection
//! across refreshes; the QUIC handshake and hole-punched path are paid
//! once, then reused by info probes and every request after.
//!
//! The wire envelope is `Result<Vec<u8>, String>` — same shape as the mint
//! API.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use anyhow::Context;
use iroh::{Endpoint, PublicKey};
use picomint_encoding::{Decodable, Encodable};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_util::task::AbortOnDropHandle;

use picomint_rpc::connection::{
    ConnState, connection_task, request_on_state, request_on_state_retry,
};

/// A counterparty the pool dials, by its iroh identity.
pub(crate) trait Peer: Ord + Copy + Send + Sync + 'static {
    fn iroh_pk(&self) -> PublicKey;
}

/// One announced counterparty: its pooled connection and the latest info
/// probe, dropped together when it leaves the announced set.
struct Member<I> {
    /// `None` until the first successful info probe. A member without info
    /// is not selectable but keeps its warm connection.
    info: Option<I>,
    conn: watch::Receiver<Option<ConnState>>,
    /// Aborts the member's [`connection_task`] when this entry is dropped.
    _task: AbortOnDropHandle<()>,
}

/// Pool of announced counterparties keyed by pk, each with a kept-alive
/// connection and its latest info. Cloneable; one instance is shared by a
/// module and its state machines.
pub(crate) struct Pool<K, I> {
    endpoint: Endpoint,
    inner: Arc<RwLock<BTreeMap<K, Member<I>>>>,
}

impl<K, I> Clone for Pool<K, I> {
    fn clone(&self) -> Self {
        Self {
            endpoint: self.endpoint.clone(),
            inner: self.inner.clone(),
        }
    }
}

impl<K: Peer, I: Clone + Send + 'static> Pool<K, I> {
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            inner: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    /// Bring the connection pool in line with the announced set `pks`:
    /// spawn a kept-alive [`connection_task`] for each pk not already
    /// pooled, leaving survivors' warm connections untouched.
    ///
    /// `prune` distinguishes the two callers. The authoritative pk refresh
    /// passes `true`: members no longer announced are dropped (their
    /// connection task aborted). The cold-start info refresh passes
    /// `false`: it only adds connections for the previous session's
    /// persisted pks, so it can run concurrently with the authoritative
    /// refresh without racing it for membership.
    pub fn reconcile(&self, pks: &[K], prune: bool) {
        let mut map = self.inner.write().expect("pool RwLock poisoned");

        if prune {
            map.retain(|pk, _| pks.contains(pk));
        }

        for pk in pks {
            map.entry(*pk).or_insert_with(|| {
                let (tx, rx) = watch::channel(None);
                let task = tokio::spawn(connection_task(pk.iroh_pk(), self.endpoint.clone(), tx));
                Member {
                    info: None,
                    conn: rx,
                    _task: AbortOnDropHandle::new(task),
                }
            });
        }
    }

    /// Probe `method` on each member in `pks` concurrently over its pooled
    /// connection, writing what `info` makes of each response back as it
    /// arrives. A failed probe clears that member's info (unselectable)
    /// without dropping its connection; a slow probe never blocks the
    /// others' updates.
    pub async fn probe<M, R>(
        &self,
        pks: &[K],
        method: M,
        info: impl Fn(R) -> Option<I> + Clone + Send + 'static,
    ) where
        M: Encodable + Clone + Send + 'static,
        R: Decodable + Send + 'static,
    {
        let connections: Vec<_> = self
            .inner
            .read()
            .expect("pool RwLock poisoned")
            .iter()
            .filter(|entry| pks.contains(entry.0))
            .map(|entry| (*entry.0, entry.1.conn.clone()))
            .collect();

        let mut probes: JoinSet<(K, Option<I>)> = JoinSet::new();

        for (pk, mut rx) in connections {
            let method = method.clone();
            let info = info.clone();

            probes.spawn(async move {
                let info = request_on_state::<R>(&mut rx, method)
                    .await
                    .ok()
                    .and_then(info);

                (pk, info)
            });
        }

        while let Some(Ok((pk, info))) = probes.join_next().await {
            if let Some(member) = self
                .inner
                .write()
                .expect("pool RwLock poisoned")
                .get_mut(&pk)
            {
                member.info = info;
            }
        }
    }

    /// Every pooled member with a successful info probe, keyed by pk.
    pub fn list(&self) -> BTreeMap<K, I> {
        self.inner
            .read()
            .expect("pool RwLock poisoned")
            .iter()
            .filter_map(|(pk, member)| member.info.clone().map(|info| (*pk, info)))
            .collect()
    }

    /// The latest probed info of `pk`, if it is pooled and probed.
    pub fn info(&self, pk: K) -> Option<I> {
        self.inner
            .read()
            .expect("pool RwLock poisoned")
            .get(&pk)
            .and_then(|member| member.info.clone())
    }

    /// Status watch for `pk`, if it is a current member.
    fn connection(&self, pk: K) -> Option<watch::Receiver<Option<ConnState>>> {
        self.inner
            .read()
            .expect("pool RwLock poisoned")
            .get(&pk)
            .map(|member| member.conn.clone())
    }

    pub async fn request<R: Decodable>(&self, pk: K, method: impl Encodable) -> anyhow::Result<R> {
        let mut rx = self
            .connection(pk)
            .context("Counterparty is not a current member")?;

        request_on_state(&mut rx, method).await
    }

    /// As [`Self::request`], retrying transport errors forever on the
    /// pooled connection; errors only if `pk` is not a current member,
    /// which no retry can cure.
    pub async fn request_retry<R: Decodable>(
        &self,
        pk: K,
        method: impl Encodable + Clone,
    ) -> anyhow::Result<R> {
        let mut rx = self
            .connection(pk)
            .context("Counterparty is not a current member")?;

        request_on_state_retry(&mut rx, method).await
    }
}
