use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use crate::Endpoint;
use crate::api::FederationApi;
use crate::gw::GatewayClientModule;
use crate::join::Join;
use crate::ln::LightningClientModule;
use crate::mint::MintClientModule;
use crate::secret::{ClientSecret, Mnemonic};
use crate::task::TaskGroup;
use crate::wallet::WalletClientModule;
use anyhow::ensure;
use futures::stream::BoxStream;
use picomint_core::Amount;
use picomint_core::PeerId;
use picomint_core::config::ConsensusConfig;
use picomint_core::config::FederationId;
use picomint_core::core::{Account, OperationId};
use picomint_core::fee::FeeConfig;
use picomint_core::invite::InviteCode;
use picomint_eventlog::{EventLogEntry, EventLogId};
use picomint_rpc::connection::ConnStatus;
use picomint_sqlite::{Database, DbRead, table};
use tracing::debug;

// The config of every joined federation. The row is what makes a federation
// joined: [`Client::join`] inserts it and [`Client::leave`] removes it,
// atomically with the federation's other rows.
table!(
    ClientConfigTable,
    FederationId => ConsensusConfig,
    "client-config",
);

/// LN-flavor selection made at [`Client`] construction and applied to every
/// federation the client brings up.
#[derive(Copy, Clone)]
enum LnChoice {
    Regular,
    Gateway,
}

/// Lightning-module flavor mounted on a federation client. Regular clients
/// use `Regular`, while the gateway daemon mounts `Gateway`. The two flavors
/// are mutually exclusive at the same federation instance.
pub(crate) enum LnFlavor {
    Regular(Arc<LightningClientModule>),
    Gateway(Arc<GatewayClientModule>),
}

/// Main client type: one instance per application, holding every joined
/// federation.
///
/// Owns the shared resources — database handle, iroh endpoint, seed, event
/// log — and a map of per-federation [`FederationClient`]s. Federations are
/// joined via [`Client::scan`] + [`Client::join`], brought up lazily by
/// [`Client::federation`] (or all at once by [`Client::warm`]), and removed
/// by [`Client::leave`]. Policy stays with the integrator: the client never
/// decides when to bring a federation up or whether leaving is allowed.
pub struct Client {
    endpoint: Endpoint,
    db: Database,
    mnemonic: Mnemonic,
    fee: Option<FeeConfig>,
    ln_choice: LnChoice,
    federations: RwLock<BTreeMap<FederationId, Arc<FederationClient>>>,
}

impl Client {
    /// Build a regular client over `db` and `endpoint`. Inert: no federation
    /// is brought up until [`Self::federation`], [`Self::warm`] or
    /// [`Self::join`] does so.
    ///
    /// `fee` is the integrator's cut: [`FeeConfig::ppm`] parts per million of
    /// the value every transaction this client builds moves, paid into
    /// [`Account::AppFee`] as an output of that same transaction and swept
    /// from there to [`FeeConfig::lnurl`] as it accumulates. `None` charges
    /// nothing and starts no sweep.
    pub fn new(
        endpoint: Endpoint,
        db: Database,
        mnemonic: Mnemonic,
        fee: Option<FeeConfig>,
    ) -> Self {
        Self::build(endpoint, db, mnemonic, fee, LnChoice::Regular)
    }

    /// Gateway-flavor counterpart of [`Client::new`]. Used by the gateway
    /// daemon, which mounts [`GatewayClientModule`] in place of the regular
    /// lightning module on every federation.
    ///
    /// Takes no cut. A gateway's transactions are the other half of its
    /// users' payments, and charging them would bill the gateway for
    /// serving the very payment the sender was already charged for.
    pub fn new_gateway(endpoint: Endpoint, db: Database, mnemonic: Mnemonic) -> Self {
        Self::build(endpoint, db, mnemonic, None, LnChoice::Gateway)
    }

    fn build(
        endpoint: Endpoint,
        db: Database,
        mnemonic: Mnemonic,
        fee: Option<FeeConfig>,
        ln_choice: LnChoice,
    ) -> Self {
        debug!(
            version = %env!("CARGO_PKG_VERSION"),
            "Building picomint client",
        );

        Self {
            endpoint,
            db,
            mnemonic,
            fee,
            ln_choice,
            federations: RwLock::new(BTreeMap::new()),
        }
    }

    /// Download a federation's config via `invite` and scan every account the
    /// seed could hold notes under. Reads nothing and writes nothing locally;
    /// inspect [`Join::config`] and hand the result to [`Self::join`] to
    /// actually join.
    pub async fn scan(&self, invite: &InviteCode) -> anyhow::Result<Join> {
        crate::join::join(&self.endpoint, &self.mnemonic, invite).await
    }

    /// Join the federation a [`Self::scan`] prepared: persist its config and
    /// the scan's counter marks and notes in one dbtx, then bring the
    /// federation up. Errors if the federation is already joined.
    pub fn join(&self, join: Join) -> anyhow::Result<Arc<FederationClient>> {
        let federation = join.config().calculate_federation_id();

        let mut federations = self.federations.write().expect("federations lock poisoned");

        let dbtx = self.db.begin_write();

        ensure!(
            dbtx.get(&ClientConfigTable, &federation).is_none(),
            "Federation is already joined"
        );

        dbtx.insert(&ClientConfigTable, &federation, join.config());
        join.commit(&dbtx);
        dbtx.commit();

        let client = FederationClient::build(self, join.config().clone());

        federations.insert(federation, client.clone());

        Ok(client)
    }

    /// The joined federation's live client, bringing it up from the persisted
    /// config on first use. Returns `None` for a federation that is not
    /// joined.
    ///
    /// Double-checked: the read-lock fast path serves the hot case, and a
    /// cache miss re-checks under the write lock in case another caller
    /// raced and inserted.
    pub fn federation(&self, federation: FederationId) -> Option<Arc<FederationClient>> {
        if let Some(client) = self
            .federations
            .read()
            .expect("federations lock poisoned")
            .get(&federation)
        {
            return Some(client.clone());
        }

        let mut federations = self.federations.write().expect("federations lock poisoned");

        if let Some(client) = federations.get(&federation) {
            return Some(client.clone());
        }

        let config = self.db.begin_read().get(&ClientConfigTable, &federation)?;

        let client = FederationClient::build(self, config);

        federations.insert(federation, client.clone());

        Some(client)
    }

    /// Every joined federation, whether or not it is currently up.
    pub fn federations(&self) -> Vec<FederationId> {
        self.db
            .begin_read()
            .iter(&ClientConfigTable, |r| r.map(|entry| entry.0).collect())
    }

    /// Every joined federation's persisted config, without bringing any up.
    pub fn federation_configs(&self) -> BTreeMap<FederationId, ConsensusConfig> {
        self.db
            .begin_read()
            .iter(&ClientConfigTable, |r| r.collect())
    }

    /// The iroh endpoint shared by every federation's connections.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// The seed every federation's secrets derive from.
    pub fn mnemonic(&self) -> &Mnemonic {
        &self.mnemonic
    }

    /// Bring up every joined federation. What an app that shows all balances
    /// at startup wants; a gateway serving federations on demand never calls
    /// this and stays lazy.
    pub fn warm(&self) {
        for federation in self.federations() {
            self.federation(federation);
        }
    }

    /// Leave a federation: shut its client down, then remove its config and
    /// every row it holds in one dbtx, so a crash mid-leave loses nothing
    /// halfway. Re-joining later runs a fresh [`Self::scan`] against clean
    /// state.
    pub async fn leave(&self, federation: FederationId) -> anyhow::Result<()> {
        let client = self
            .federations
            .write()
            .expect("federations lock poisoned")
            .remove(&federation);

        // Wait for every task to observe cancellation before the wipe, so no
        // state machine is mid-write while its rows disappear.
        if let Some(client) = client {
            client.shutdown().await;
        }

        let dbtx = self.db.begin_write();

        ensure!(
            dbtx.remove(&ClientConfigTable, &federation).is_some(),
            "Federation is not joined"
        );

        crate::mint::wipe_tables(&dbtx, federation);
        crate::wallet::wipe_tables(&dbtx, federation);
        crate::ln::wipe_tables(&dbtx, federation);
        crate::gw::wipe_tables(&dbtx, federation);
        crate::tx::wipe_tables(&dbtx, federation);
        crate::expiry::wipe_tables(&dbtx, federation);

        dbtx.commit();

        Ok(())
    }

    /// Cancel every federation's tasks and wait for them to finish.
    pub async fn shutdown(&self) {
        let federations =
            std::mem::take(&mut *self.federations.write().expect("federations lock poisoned"));

        for client in federations.into_values() {
            client.shutdown().await;
        }
    }

    pub fn db(&self) -> &Database {
        &self.db
    }

    pub fn get_event_log(&self, pos: EventLogId, limit: u64) -> Vec<(EventLogId, EventLogEntry)> {
        picomint_eventlog::get_event_log(&self.db, pos, limit)
    }

    /// Shared [`Notify`] that fires on every commit touching the event log.
    pub fn event_notify(&self) -> Arc<tokio::sync::Notify> {
        picomint_eventlog::event_notify(&self.db)
    }

    /// One-shot snapshot of every event currently logged for `operation`,
    /// in insertion order.
    pub fn read_operation_events(&self, operation: OperationId) -> Vec<EventLogEntry> {
        picomint_eventlog::read_operation_events(&self.db, operation)
    }

    /// Stream every event belonging to `operation`, starting from the
    /// beginning of the log (existing events first, then live ones).
    pub fn subscribe_operation_events(
        &self,
        operation: OperationId,
    ) -> BoxStream<'static, EventLogEntry> {
        Box::pin(picomint_eventlog::subscribe_operation_events(
            self.db.clone(),
            operation,
        ))
    }
}

/// A handle and API to interacting with a single joined federation, obtained
/// from [`Client::federation`] or [`Client::join`].
///
/// Under the hood it owns service tasks, state machines, and module handles.
/// Dropping the last [`Arc<FederationClient>`] cancels all spawned tasks
/// (best-effort, non-blocking); [`Client::leave`] and [`Client::shutdown`]
/// wait for them instead.
pub struct FederationClient {
    config: ConsensusConfig,
    db: Database,
    federation: FederationId,
    pub(crate) mint: Arc<MintClientModule>,
    pub(crate) wallet: Arc<WalletClientModule>,
    pub(crate) ln: LnFlavor,
    pub(crate) api: FederationApi,
    tg: TaskGroup,
}

impl FederationClient {
    /// Bring up a federation client against `config`.
    ///
    /// Not inert: the modules spawn their state-machine executors — resuming
    /// whatever the database already holds — and the background refreshes
    /// commit writes of their own. So this goes last, after the dbtx that
    /// persists `config` and the join's scan results.
    fn build(client: &Client, config: ConsensusConfig) -> Arc<Self> {
        let federation = config.calculate_federation_id();
        let client_secret = ClientSecret::new(&client.mnemonic, federation);

        let peer_node_ids: BTreeMap<PeerId, iroh_base::PublicKey> = config
            .peers
            .iter()
            .map(|(peer, endpoint)| (*peer, endpoint.iroh_pk))
            .collect();
        let api: FederationApi = FederationApi::new(client.endpoint.clone(), peer_node_ids);

        let tg = TaskGroup::new();

        let mint_context =
            crate::module::ClientContext::new(api.clone(), client.db.clone(), config.clone());
        let mint = Arc::new(MintClientModule::new(
            federation,
            config.mint.clone(),
            mint_context,
            client_secret.mint_secret(),
            client.fee.as_ref().map_or(0, |fee| fee.ppm),
            &tg,
        ));

        let wallet_context =
            crate::module::ClientContext::new(api.clone(), client.db.clone(), config.clone());
        let wallet = Arc::new(WalletClientModule::new(
            config.wallet.clone(),
            wallet_context,
            mint.clone(),
            client_secret.wallet_secret(),
            &tg,
        ));

        let ln = match client.ln_choice {
            LnChoice::Regular => {
                let ln_context = crate::module::ClientContext::new(
                    api.clone(),
                    client.db.clone(),
                    config.clone(),
                );
                LnFlavor::Regular(Arc::new(LightningClientModule::new(
                    federation,
                    config.ln.clone(),
                    ln_context,
                    mint.clone(),
                    client_secret.ln_secret(),
                    &tg,
                )))
            }
            LnChoice::Gateway => {
                let gw_context = crate::module::ClientContext::new(
                    api.clone(),
                    client.db.clone(),
                    config.clone(),
                );
                LnFlavor::Gateway(Arc::new(GatewayClientModule::new(
                    federation,
                    config.ln.clone(),
                    gw_context,
                    mint.clone(),
                    client_secret.gw_secret(),
                    &tg,
                )))
            }
        };

        let federation_client = Arc::new(FederationClient {
            config,
            db: client.db.clone(),
            federation,
            mint,
            wallet,
            ln,
            api,
            tg,
        });

        federation_client
            .tg
            .spawn(Self::refresh_expiry_status(federation_client.clone()));

        // Only when there is a cut to collect: a client that charges nothing
        // has nothing accruing in the account, and a sweep would wake every
        // half minute to read a balance that is always zero.
        if let Some(fee) = client.fee.clone() {
            federation_client.tg.spawn(crate::fee::sweep(
                federation_client.clone(),
                Account::AppFee,
                fee.lnurl,
            ));
        }

        federation_client
    }

    /// Cancel all spawned tasks and wait for them to finish. No timeout —
    /// blocks until every state machine driver and background task has
    /// observed cancellation and exited cleanly.
    pub async fn shutdown(&self) {
        self.tg.shutdown().await;
    }

    pub fn api(&self) -> &FederationApi {
        &self.api
    }

    /// Stream of per-peer guardian reachability, emitting a fresh
    /// `peer -> status` map on every change (current state first). Backed by
    /// the client's pooled connections, so it reflects the same links requests
    /// travel over; the `Connected` status carries the RTT sampled at connect.
    pub fn connection_status_stream(&self) -> BoxStream<'static, BTreeMap<PeerId, ConnStatus>> {
        self.api.connection_status_stream()
    }

    pub fn federation(&self) -> FederationId {
        self.federation
    }

    pub fn config(&self) -> &ConsensusConfig {
        &self.config
    }

    pub fn mint(&self) -> &MintClientModule {
        &self.mint
    }

    pub fn wallet(&self) -> &WalletClientModule {
        &self.wallet
    }

    /// Regular-flavor lightning module. Panics if this client mounts the
    /// gateway flavor instead.
    pub fn ln(&self) -> &LightningClientModule {
        match &self.ln {
            LnFlavor::Regular(m) => m,
            LnFlavor::Gateway(_) => panic!("LightningClientModule is not mounted on this client"),
        }
    }

    /// Gateway-flavor lightning module. Panics if this client mounts the
    /// regular flavor instead.
    pub fn gw(&self) -> &GatewayClientModule {
        match &self.ln {
            LnFlavor::Gateway(m) => m,
            LnFlavor::Regular(_) => panic!("GatewayClientModule is not mounted on this client"),
        }
    }

    pub fn db(&self) -> &Database {
        &self.db
    }

    pub fn get_balance(&self, account: Account) -> Amount {
        self.mint.get_balance(&self.db().begin_read(), account)
    }

    /// Yields `account`'s balance whenever the note table is written. The same
    /// value may be yielded repeatedly — every federation's accounts share one
    /// table, so writes to another account or federation wake this stream too.
    pub fn subscribe_balance_changes(&self, account: Account) -> BoxStream<'static, Amount> {
        let notify = self.mint.balance_notify();
        let mint = self.mint.clone();
        let db = self.db().clone();

        Box::pin(async_stream::stream! {
            loop {
                // Registered before the read so a write landing in between
                // still wakes the already-registered waiter.
                let notified = notify.notified();

                yield mint.get_balance(&db.begin_read(), account);

                notified.await;
            }
        })
    }

    /// Returns a list of guardian iroh API node ids
    pub fn get_peer_node_ids(&self) -> BTreeMap<PeerId, iroh_base::PublicKey> {
        self.config()
            .peers
            .iter()
            .map(|(peer, endpoint)| (*peer, endpoint.iroh_pk))
            .collect()
    }

    /// Returns the guardian public key set from the client config.
    pub fn get_guardian_public_keys_blocking(
        &self,
    ) -> BTreeMap<PeerId, picomint_core::secp256k1::XOnlyPublicKey> {
        self.config()
            .peers
            .iter()
            .map(|(peer, endpoint)| (*peer, endpoint.broadcast_pk))
            .collect()
    }
}

/// Cancel-only on drop. Spawned tasks observe the cancellation token at
/// the next await and unwind. Callers wanting to wait for tasks to
/// complete should `client.shutdown().await` first.
impl Drop for FederationClient {
    fn drop(&mut self) {
        self.tg.cancel();
    }
}
