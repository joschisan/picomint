use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use crate::Endpoint;
use crate::api::FederationApi;
use crate::context::ClientContext;
use crate::eventlog::{EventLogEntry, EventLogId};
use crate::ln::Gateways;
use crate::secret::{ClientSecret, Mnemonic};
use crate::task::TaskGroup;
use anyhow::{Context as _, ensure};
use futures::future::select_all;
use futures::stream::BoxStream;
use picomint_core::PeerId;
use picomint_core::config::ConsensusConfig;
use picomint_core::config::FederationId;
use picomint_core::core::OperationId;
use picomint_core::invite::InviteCode;
use picomint_redb::{Database, DbRead, WriteTx, table};
use picomint_rpc::connection::ConnStatus;
use tracing::debug;

// The config of every added federation. The row is what makes a federation
// added: [`Client::add`] inserts it and [`Client::begin_remove`] removes it,
// atomically with the federation's other rows.
table!(
    ClientConfigTable,
    FederationId => ConsensusConfig,
    "client-config",
);

/// Main client type: one instance per application, holding every added
/// federation as data.
///
/// Owns the shared resources — database handle, iroh endpoint, seed, event
/// log — and a private map of per-federation runtimes. An added federation
/// is always up: [`Client::new`] brings every one up at construction,
/// [`Client::add`] brings a new one up on add, and [`Client::begin_remove`]
/// shuts one down as it removes it — there is no dormant state in between.
/// Every operation takes the [`FederationId`] it acts on; there is no
/// per-federation handle to hold or leak. Policy stays with the integrator:
/// the client never decides whether removing is allowed.
///
/// Teardown is explicit: call [`Self::shutdown`] before letting go of the
/// client. Dropping it without one leaks the added federations' tasks until
/// process exit.
pub struct Client {
    pub(crate) endpoint: Endpoint,
    pub(crate) db: Database,
    pub(crate) mnemonic: Mnemonic,
    federations: RwLock<BTreeMap<FederationId, ClientContext>>,
}

impl Client {
    /// Build a client over the embedder's `endpoint`, `db` and seed, and
    /// bring every added federation up. Must run inside a tokio runtime,
    /// since bringing a federation up spawns its background tasks.
    ///
    /// Seed storage is the embedder's job — pass the same mnemonic on every
    /// start. The endpoint is the embedder's network identity choice: a
    /// wallet app binds one without a secret key (fresh random identity per
    /// launch — nothing dials a wallet), while the gateway daemon binds its
    /// persisted iroh key so the `GatewayPk` clients connect to survives
    /// restarts.
    ///
    pub fn new(endpoint: Endpoint, db: Database, mnemonic: Mnemonic) -> Client {
        debug!(
            version = %env!("CARGO_PKG_VERSION"),
            "Building picomint client",
        );

        let federations = db
            .begin_read()
            .iter(&ClientConfigTable, |r| r.collect::<Vec<_>>())
            .into_iter()
            .map(|entry| (entry.0, build_ctx(&endpoint, &db, &mnemonic, entry.1)))
            .collect();

        Client {
            endpoint,
            db,
            mnemonic,
            federations: RwLock::new(federations),
        }
    }

    /// Add the federation behind `invite`: download its config, verify it
    /// against `network` if given, scan every account the seed could hold
    /// notes under, land config, counter marks and restored notes in one
    /// dbtx, and bring the federation up.
    ///
    /// The scan matters as much on a first add: a seed that has been here
    /// before holds notes behind counters a fresh client would re-derive
    /// from zero, stranding them. A seed that never held anything scans to
    /// nothing, which costs a round trip and is otherwise indistinguishable.
    pub async fn add(
        &self,
        invite: &InviteCode,
        network: Option<bitcoin::Network>,
    ) -> anyhow::Result<FederationId> {
        let (config, restores) = crate::add::add(self, invite, network).await?;

        let federation = config.calculate_federation_id();

        let dbtx = self.db.begin_write();

        ensure!(
            dbtx.get(&ClientConfigTable, &federation).is_none(),
            "Federation is already added"
        );

        dbtx.insert(&ClientConfigTable, &federation, &config);

        for (account, restore) in &restores {
            crate::ecash::commit_scan(&dbtx, *account, restore);
        }

        dbtx.commit();

        let ctx = build_ctx(&self.endpoint, &self.db, &self.mnemonic, config);

        self.federations
            .write()
            .expect("federations lock poisoned")
            .insert(federation, ctx);

        Ok(federation)
    }

    /// Remove a federation: shut its runtime down, then remove its config
    /// and every row it holds in the returned dbtx — the caller commits it,
    /// so a crash mid-remove loses nothing halfway, and an embedder sharing
    /// the database can delete its own federation-scoped rows in the same
    /// transaction: an embedder row referencing a federation then always
    /// implies the federation is added. Re-adding later runs a fresh scan
    /// against clean state.
    ///
    /// The shutdown must complete before the dbtx opens — a task blocked in
    /// `begin_write` cannot observe cancellation — which is why this method
    /// owns the ordering and hands back the open tx rather than accepting
    /// one.
    pub async fn begin_remove(&self, federation: FederationId) -> anyhow::Result<WriteTx> {
        let ctx = self
            .federations
            .write()
            .expect("federations lock poisoned")
            .remove(&federation);

        // Wait for every task to observe cancellation before the wipe, so no
        // state machine is mid-write while its rows disappear.
        if let Some(ctx) = ctx {
            ctx.tg.shutdown().await;
        }

        let dbtx = self.db.begin_write();

        ensure!(
            dbtx.remove(&ClientConfigTable, &federation).is_some(),
            "Federation is not added"
        );

        crate::ecash::wipe_tables(&dbtx, federation);
        crate::wallet::wipe_tables(&dbtx, federation);
        crate::ln::wipe_tables(&dbtx, federation);
        crate::gw::wipe_tables(&dbtx, federation);
        crate::tx::wipe_tables(&dbtx, federation);
        crate::expiry::wipe_tables(&dbtx, federation);

        Ok(dbtx)
    }

    /// The added federation's context. Errors for a federation that is not
    /// added.
    pub(crate) fn ctx(&self, federation: FederationId) -> anyhow::Result<ClientContext> {
        self.federations
            .read()
            .expect("federations lock poisoned")
            .get(&federation)
            .cloned()
            .context("Federation is not added")
    }

    /// Whether `federation` is added — the membership check without
    /// [`Self::ctx`]'s context clone.
    pub(crate) fn is_added(&self, federation: FederationId) -> bool {
        self.federations
            .read()
            .expect("federations lock poisoned")
            .contains_key(&federation)
    }

    /// Every added federation.
    pub fn federations(&self) -> Vec<FederationId> {
        self.db
            .begin_read()
            .iter(&ClientConfigTable, |r| r.map(|entry| entry.0).collect())
    }

    /// Every added federation's persisted config.
    pub fn federation_configs(&self) -> BTreeMap<FederationId, ConsensusConfig> {
        self.db
            .begin_read()
            .iter(&ClientConfigTable, |r| r.collect())
    }

    /// The added federation's persisted config.
    pub fn config(&self, federation: FederationId) -> Option<ConsensusConfig> {
        self.db.begin_read().get(&ClientConfigTable, &federation)
    }

    /// Stream of per-peer guardian reachability, emitting a fresh
    /// `peer -> status` map on every change (current state first). Backed by
    /// the federation's pooled connections, so it reflects the same links
    /// requests travel over; the `Connected` status carries the RTT sampled
    /// at connect.
    pub fn connection_status_stream(
        &self,
        federation: FederationId,
    ) -> anyhow::Result<BoxStream<'static, BTreeMap<PeerId, ConnStatus>>> {
        Ok(self.ctx(federation)?.api.connection_status_stream())
    }

    /// The federation's API handle.
    pub fn api(&self, federation: FederationId) -> anyhow::Result<FederationApi> {
        Ok(self.ctx(federation)?.api.clone())
    }

    /// The consensus block count of the federation.
    pub async fn block_count(&self, federation: FederationId) -> anyhow::Result<u32> {
        crate::api::block_count(&self.ctx(federation)?.api).await
    }

    /// Cancel every federation's tasks and wait for them to finish.
    pub async fn shutdown(&self) {
        let federations =
            std::mem::take(&mut *self.federations.write().expect("federations lock poisoned"));

        for ctx in federations.into_values() {
            ctx.tg.shutdown().await;
        }
    }

    pub fn get_event_log(&self, pos: EventLogId, limit: u64) -> Vec<(EventLogId, EventLogEntry)> {
        crate::eventlog::get_event_log(&self.db, pos, limit)
    }

    /// Shared [`Notify`] that fires on every commit touching the event log.
    pub fn event_notify(&self) -> Arc<tokio::sync::Notify> {
        crate::eventlog::event_notify(&self.db)
    }

    /// One-shot snapshot of every event currently logged for `operation`,
    /// in insertion order.
    pub fn read_operation_events(&self, operation: OperationId) -> Vec<EventLogEntry> {
        crate::eventlog::read_operation_events(&self.db, operation)
    }

    /// Whether any state machine is still driving `operation` under
    /// `federation`. The synchronous companion to
    /// [`Self::subscribe_completion`]: read the current state with this,
    /// subscribe to the transition with that — a subscription alone leaves
    /// the caller guessing until its first resolution arrives.
    pub fn operation_is_active(&self, federation: FederationId, operation: OperationId) -> bool {
        let dbtx = self.db.begin_read();

        crate::tx::operation_is_active(&dbtx, federation, operation)
            || crate::ecash::operation_is_active(&dbtx, federation, operation)
            || crate::ln::operation_is_active(&dbtx, federation, operation)
            || crate::wallet::operation_is_active(&dbtx, federation, operation)
            || crate::gw::operation_is_active(&dbtx, federation, operation)
    }

    /// Resolve once no state machine is still driving `operation` under
    /// `federation`. Resolves immediately for a settled or unknown
    /// operation.
    ///
    /// This answers "is anything still running", not "did the payment
    /// succeed": the outcome is carried by the event log, and a receive
    /// that is waiting on its payer has no state machine yet and reads as
    /// complete. State machines transition atomically, so once this
    /// resolves the event log already holds every event the operation's
    /// state machines logged — including the terminal one, committed in
    /// the same tx that removed its state machine.
    ///
    /// Purely db-backed: for a federation that is no longer added it simply
    /// stays pending, since nothing is driving the operation forward.
    pub async fn subscribe_completion(&self, federation: FederationId, operation: OperationId) {
        let notifies = [
            crate::tx::sm_notifies(&self.db),
            crate::ecash::sm_notifies(&self.db),
            crate::ln::sm_notifies(&self.db),
            crate::wallet::sm_notifies(&self.db),
            crate::gw::sm_notifies(&self.db),
        ]
        .concat();

        loop {
            // Armed before the check: `Notified` captures the generation at
            // construction, so a commit landing between check and await
            // still wakes us.
            let notified: Vec<_> = notifies
                .iter()
                .map(|notify| Box::pin(notify.notified()))
                .collect();

            if !self.operation_is_active(federation, operation) {
                return;
            }

            select_all(notified).await;
        }
    }

    /// Stream every event belonging to `operation`, starting from the
    /// beginning of the log (existing events first, then live ones).
    pub fn subscribe_operation_events(
        &self,
        operation: OperationId,
    ) -> BoxStream<'static, EventLogEntry> {
        Box::pin(crate::eventlog::subscribe_operation_events(
            self.db.clone(),
            operation,
        ))
    }
}

/// Bring up a federation against `config`.
///
/// Not inert: resuming the persisted state machines and the background
/// refreshes commit writes of their own. So this goes last, after the
/// dbtx that persists `config` and the add's scan results — and the
/// resumes run before the context is published in the federation map,
/// so no concurrent operation can add a state machine mid-resume.
fn build_ctx(
    endpoint: &Endpoint,
    db: &Database,
    mnemonic: &Mnemonic,
    config: ConsensusConfig,
) -> ClientContext {
    let federation = config.calculate_federation_id();

    let ctx = ClientContext::new(
        FederationApi::new(endpoint.clone(), config.iroh_pks()),
        db.clone(),
        config,
        ClientSecret::new(mnemonic, federation),
        Gateways::new(endpoint.clone()),
        TaskGroup::new(),
    );

    crate::ecash::resume(&ctx);

    crate::wallet::resume(&ctx);

    crate::ln::resume(&ctx);

    crate::gw::resume(&ctx);

    ctx.tg.spawn(crate::expiry::refresh(ctx.clone()));

    ctx
}
