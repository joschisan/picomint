use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use crate::Endpoint;
use crate::api::MintApi;
use crate::context::ClientContext;
use crate::eventlog::{EventLogEntry, EventLogId};
use crate::lightning::Gateways;
use crate::secret::{ClientSecret, Mnemonic};
use crate::task::TaskGroup;
use anyhow::{Context as _, ensure};
use futures::future::select_all;
use futures::stream::BoxStream;
use picomint_core::NodeId;
use picomint_core::config::ConsensusConfig;
use picomint_core::config::MintId;
use picomint_core::core::OperationId;
use picomint_core::invite::InviteCode;
use picomint_redb::{Database, DbRead, WriteTx, table};
use picomint_rpc::connection::ConnStatus;
use tracing::debug;

// The config of every added mint. The row is what makes a mint
// added: [`Client::add_mint`] inserts it and [`Client::begin_remove_mint`] removes it,
// atomically with the mint's other rows.
table!(
    ClientConfigTable,
    MintId => ConsensusConfig,
    "client-config",
);

/// Main client type: one instance per application, holding every added
/// mint as data.
///
/// Owns the shared resources — database handle, iroh endpoint, seed, event
/// log — and a private map of per-mint runtimes. An added mint
/// is always up: [`Client::new`] brings every one up at construction,
/// [`Client::add_mint`] brings a new one up on add, and [`Client::begin_remove_mint`]
/// shuts one down as it removes it — there is no dormant state in between.
/// Every operation takes the [`MintId`] it acts on; there is no
/// per-mint handle to hold or leak. Policy stays with the integrator:
/// the client never decides whether removing is allowed.
///
/// Teardown is explicit: call [`Self::shutdown`] before letting go of the
/// client. Dropping it without one leaks the added mints' tasks until
/// process exit.
pub struct Client {
    pub(crate) endpoint: Endpoint,
    pub(crate) db: Database,
    pub(crate) mnemonic: Mnemonic,
    mints: RwLock<BTreeMap<MintId, ClientContext>>,
}

impl Client {
    /// Build a client over the embedder's `endpoint`, `db` and seed, and
    /// bring every added mint up. Must run inside a tokio runtime,
    /// since bringing a mint up spawns its background tasks.
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

        let mints = db
            .begin_read()
            .iter(&ClientConfigTable, |r| r.collect::<Vec<_>>())
            .into_iter()
            .map(|entry| (entry.0, build_ctx(&endpoint, &db, &mnemonic, entry.1)))
            .collect();

        Client {
            endpoint,
            db,
            mnemonic,
            mints: RwLock::new(mints),
        }
    }

    /// Add the mint behind `invite`: download its config, verify it
    /// against `network` if given, scan every account the seed could hold
    /// notes under, land config, counter marks and restored notes in one
    /// dbtx, and bring the mint up.
    ///
    /// The scan matters as much on a first add: a seed that has been here
    /// before holds notes behind counters a fresh client would re-derive
    /// from zero, stranding them. A seed that never held anything scans to
    /// nothing, which costs a round trip and is otherwise indistinguishable.
    pub async fn add_mint(
        &self,
        invite: &InviteCode,
        network: Option<bitcoin::Network>,
    ) -> anyhow::Result<MintId> {
        let (config, restores) = crate::add_mint::add_mint(self, invite, network).await?;

        let mint = config.calculate_mint_id();

        let dbtx = self.db.begin_write();

        ensure!(
            dbtx.get(&ClientConfigTable, &mint).is_none(),
            "Mint is already added"
        );

        dbtx.insert(&ClientConfigTable, &mint, &config);

        for (account, restore) in &restores {
            crate::ecash::commit_scan(&dbtx, *account, restore);
        }

        dbtx.commit();

        let ctx = build_ctx(&self.endpoint, &self.db, &self.mnemonic, config);

        self.mints
            .write()
            .expect("mints lock poisoned")
            .insert(mint, ctx);

        Ok(mint)
    }

    /// Remove a mint: shut its runtime down, then remove its config
    /// and every row it holds in the returned dbtx — the caller commits it,
    /// so a crash mid-remove loses nothing halfway, and an embedder sharing
    /// the database can delete its own mint-scoped rows in the same
    /// transaction: an embedder row referencing a mint then always
    /// implies the mint is added. Re-adding later runs a fresh scan
    /// against clean state.
    ///
    /// The shutdown must complete before the dbtx opens — a task blocked in
    /// `begin_write` cannot observe cancellation — which is why this method
    /// owns the ordering and hands back the open tx rather than accepting
    /// one.
    pub async fn begin_remove_mint(&self, mint: MintId) -> anyhow::Result<WriteTx> {
        let ctx = self
            .mints
            .write()
            .expect("mints lock poisoned")
            .remove(&mint);

        // Wait for every task to observe cancellation before the wipe, so no
        // state machine is mid-write while its rows disappear.
        if let Some(ctx) = ctx {
            ctx.tg.shutdown().await;
        }

        let dbtx = self.db.begin_write();

        ensure!(
            dbtx.remove(&ClientConfigTable, &mint).is_some(),
            "Mint is not added"
        );

        crate::ecash::wipe_tables(&dbtx, mint);
        crate::onchain::wipe_tables(&dbtx, mint);
        crate::lightning::wipe_tables(&dbtx, mint);
        crate::gateway::wipe_tables(&dbtx, mint);
        crate::tx::wipe_tables(&dbtx, mint);
        crate::expiry::wipe_tables(&dbtx, mint);

        Ok(dbtx)
    }

    /// The added mint's context. Errors for a mint that is not
    /// added.
    pub(crate) fn ctx(&self, mint: MintId) -> anyhow::Result<ClientContext> {
        self.mints
            .read()
            .expect("mints lock poisoned")
            .get(&mint)
            .cloned()
            .context("Mint is not added")
    }

    /// Whether `mint` is added — the membership check without
    /// [`Self::ctx`]'s context clone.
    pub(crate) fn is_added(&self, mint: MintId) -> bool {
        self.mints
            .read()
            .expect("mints lock poisoned")
            .contains_key(&mint)
    }

    /// Every added mint.
    pub fn mints(&self) -> Vec<MintId> {
        self.db
            .begin_read()
            .iter(&ClientConfigTable, |r| r.map(|entry| entry.0).collect())
    }

    /// Every added mint's persisted config.
    pub fn mint_configs(&self) -> BTreeMap<MintId, ConsensusConfig> {
        self.db
            .begin_read()
            .iter(&ClientConfigTable, |r| r.collect())
    }

    /// The added mint's persisted config.
    pub fn config(&self, mint: MintId) -> Option<ConsensusConfig> {
        self.db.begin_read().get(&ClientConfigTable, &mint)
    }

    /// Stream of per-node guardian reachability, emitting a fresh
    /// `node -> status` map on every change (current state first). Backed by
    /// the mint's pooled connections, so it reflects the same links
    /// requests travel over; the `Connected` status carries the RTT sampled
    /// at connect.
    pub fn connection_status_stream(
        &self,
        mint: MintId,
    ) -> anyhow::Result<BoxStream<'static, BTreeMap<NodeId, ConnStatus>>> {
        Ok(self.ctx(mint)?.api.connection_status_stream())
    }

    /// The mint's API handle.
    pub fn api(&self, mint: MintId) -> anyhow::Result<MintApi> {
        Ok(self.ctx(mint)?.api.clone())
    }

    /// The consensus block count of the mint.
    pub async fn block_count(&self, mint: MintId) -> anyhow::Result<u32> {
        crate::api::block_count(&self.ctx(mint)?.api).await
    }

    /// Cancel every mint's tasks and wait for them to finish.
    pub async fn shutdown(&self) {
        let mints =
            std::mem::take(&mut *self.mints.write().expect("mints lock poisoned"));

        for ctx in mints.into_values() {
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
    /// `mint`. The synchronous companion to
    /// [`Self::subscribe_completion`]: read the current state with this,
    /// subscribe to the transition with that — a subscription alone leaves
    /// the caller guessing until its first resolution arrives.
    pub fn operation_is_active(&self, mint: MintId, operation: OperationId) -> bool {
        let dbtx = self.db.begin_read();

        crate::tx::operation_is_active(&dbtx, mint, operation)
            || crate::ecash::operation_is_active(&dbtx, mint, operation)
            || crate::lightning::operation_is_active(&dbtx, mint, operation)
            || crate::onchain::operation_is_active(&dbtx, mint, operation)
            || crate::gateway::operation_is_active(&dbtx, mint, operation)
    }

    /// Resolve once no state machine is still driving `operation` under
    /// `mint`. Resolves immediately for a settled or unknown
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
    /// Purely db-backed: for a mint that is no longer added it simply
    /// stays pending, since nothing is driving the operation forward.
    pub async fn subscribe_completion(&self, mint: MintId, operation: OperationId) {
        let notifies = [
            crate::tx::sm_notifies(&self.db),
            crate::ecash::sm_notifies(&self.db),
            crate::lightning::sm_notifies(&self.db),
            crate::onchain::sm_notifies(&self.db),
            crate::gateway::sm_notifies(&self.db),
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

            if !self.operation_is_active(mint, operation) {
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

/// Bring up a mint against `config`.
///
/// Not inert: resuming the persisted state machines and the background
/// refreshes commit writes of their own. So this goes last, after the
/// dbtx that persists `config` and the add's scan results — and the
/// resumes run before the context is published in the mint map,
/// so no concurrent operation can add a state machine mid-resume.
fn build_ctx(
    endpoint: &Endpoint,
    db: &Database,
    mnemonic: &Mnemonic,
    config: ConsensusConfig,
) -> ClientContext {
    let mint = config.calculate_mint_id();

    let ctx = ClientContext::new(
        MintApi::new(endpoint.clone(), config.iroh_pks()),
        db.clone(),
        config,
        ClientSecret::new(mnemonic, mint),
        Gateways::new(endpoint.clone()),
        TaskGroup::new(),
    );

    crate::ecash::resume(&ctx);

    crate::onchain::resume(&ctx);

    crate::lightning::resume(&ctx);

    crate::gateway::resume(&ctx);

    ctx.tg.spawn(crate::expiry::refresh(ctx.clone()));

    ctx
}
