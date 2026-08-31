use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use crate::Endpoint;
use crate::api::FederationApi;
use crate::gw::GatewayClientModule;
use crate::ln::LightningClientModule;
use crate::mint::MintClientModule;
use crate::secret::{ClientSecret, Mnemonic};
use crate::task::TaskGroup;
use crate::wallet::WalletClientModule;
use anyhow::{Context as _, ensure};
use futures::stream::BoxStream;
use iroh::endpoint::presets::N0;
use iroh_mdns_address_lookup::MdnsAddressLookup;
use picomint_core::PeerId;
use picomint_core::config::ConsensusConfig;
use picomint_core::config::FederationId;
use picomint_core::core::{Account, OperationId};
use picomint_core::fee::FeeConfig;
use picomint_core::invite::InviteCode;
use picomint_core::secret::Secret;
use picomint_eventlog::{EventLogEntry, EventLogId};
use picomint_rpc::connection::ConnStatus;
use picomint_sqlite::{Database, DbRead, table};
use tracing::debug;

// The config of every joined federation. The row is what makes a federation
// joined: [`Client::add`] inserts it and [`Client::remove`] removes it,
// atomically with the federation's other rows.
table!(
    ClientConfigTable,
    FederationId => ConsensusConfig,
    "client-config",
);

/// Main client type: one instance per application, holding every joined
/// federation as data.
///
/// Owns the shared resources — database handle, iroh endpoint, seed, event
/// log — and a private map of per-federation runtimes. Federations are
/// joined via [`Client::add`], brought up explicitly by [`Client::connect`]
/// or implicitly by the first operation that needs them, and removed by
/// [`Client::remove`]. Every operation takes the [`FederationId`] it acts
/// on; there is no per-federation handle to hold or leak. Policy stays with
/// the integrator: the client never decides when to bring a federation up
/// or whether removing is allowed.
pub struct Client {
    endpoint: Endpoint,
    pub(crate) db: Database,
    mnemonic: Mnemonic,
    pub(crate) fee: Option<FeeConfig>,
    federations: RwLock<BTreeMap<FederationId, Arc<FederationRuntime>>>,
}

impl Client {
    /// Build a client over `db` and the seed, binding the iroh endpoint from
    /// the seed's derived secret key. Inert: no federation is brought up
    /// until [`Self::connect`] or the first operation does so.
    ///
    /// Seed storage is the embedder's job — pass the same mnemonic on every
    /// start. A wallet app binds an ephemeral `api_addr` (`0.0.0.0:0`); the
    /// gateway daemon passes its stable public address and serves its API on
    /// [`Self::endpoint`].
    ///
    /// `fee` is the integrator's cut: [`FeeConfig::ppm`] parts per million of
    /// the value every transaction this client builds moves, paid into
    /// [`Account::AppFee`] as an output of that same transaction and swept
    /// from there to [`FeeConfig::lnurl`] as it accumulates. `None` charges
    /// nothing and starts no sweep — which is what a gateway passes, since
    /// its transactions are the other half of its users' payments.
    pub async fn new(
        db: Database,
        mnemonic: Mnemonic,
        api_addr: SocketAddr,
        fee: Option<FeeConfig>,
    ) -> anyhow::Result<Client> {
        debug!(
            version = %env!("CARGO_PKG_VERSION"),
            "Building picomint client",
        );

        let iroh_sk = Secret::new_root(&mnemonic.to_entropy()).to_iroh_secret_key();

        let endpoint = Endpoint::builder(N0)
            .secret_key(iroh_sk)
            .alpns(vec![picomint_rpc::ALPN.to_vec()])
            .bind_addr(api_addr)?
            .address_lookup(MdnsAddressLookup::builder())
            .bind()
            .await?;

        Ok(Client {
            endpoint,
            db,
            mnemonic,
            fee,
            federations: RwLock::new(BTreeMap::new()),
        })
    }

    /// Join the federation behind `invite`: download its config, verify it
    /// against `network` if given, scan every account the seed could hold
    /// notes under, and land config, counter marks and restored notes in one
    /// dbtx. Inert — no executor is started and no guardian connection kept;
    /// call [`Self::connect`] (or any operation) to bring the federation up.
    ///
    /// The scan matters as much on a first join: a seed that has been here
    /// before holds notes behind counters a fresh client would re-derive
    /// from zero, stranding them. A seed that never held anything scans to
    /// nothing, which costs a round trip and is otherwise indistinguishable.
    pub async fn add(
        &self,
        invite: &InviteCode,
        network: Option<bitcoin::Network>,
    ) -> anyhow::Result<FederationId> {
        let (config, restores) =
            crate::join::join(&self.endpoint, &self.mnemonic, invite, network).await?;

        let federation = config.calculate_federation_id();

        let dbtx = self.db.begin_write();

        ensure!(
            dbtx.get(&ClientConfigTable, &federation).is_none(),
            "Federation is already joined"
        );

        dbtx.insert(&ClientConfigTable, &federation, &config);

        for (account, restore) in &restores {
            crate::mint::commit_scan(&dbtx, *account, restore);
        }

        dbtx.commit();

        Ok(federation)
    }

    /// Bring `federation` up: resume its state machines, spawn its refresh
    /// loops, and build its connection pool. Idempotent — a federation that
    /// is already up is left alone.
    ///
    /// An app that shows every balance at startup calls this for each joined
    /// federation; a gateway serving federations on demand never calls it
    /// and relies on operations connecting lazily, so a dormant federation
    /// costs no connections.
    pub fn connect(&self, federation: FederationId) -> anyhow::Result<()> {
        self.runtime(federation).map(|_| ())
    }

    /// Remove a federation: shut its runtime down, then remove its config
    /// and every row it holds in one dbtx, so a crash mid-remove loses
    /// nothing halfway. Re-joining later runs a fresh scan against clean
    /// state.
    pub async fn remove(&self, federation: FederationId) -> anyhow::Result<()> {
        let runtime = self
            .federations
            .write()
            .expect("federations lock poisoned")
            .remove(&federation);

        // Wait for every task to observe cancellation before the wipe, so no
        // state machine is mid-write while its rows disappear.
        if let Some(runtime) = runtime {
            runtime.tg.shutdown().await;
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

    /// The joined federation's runtime, brought up from the persisted config
    /// on first use. Errors for a federation that is not joined.
    ///
    /// Double-checked: the read-lock fast path serves the hot case, and a
    /// cache miss re-checks under the write lock in case another caller
    /// raced and inserted.
    pub(crate) fn runtime(
        &self,
        federation: FederationId,
    ) -> anyhow::Result<Arc<FederationRuntime>> {
        if let Some(runtime) = self
            .federations
            .read()
            .expect("federations lock poisoned")
            .get(&federation)
        {
            return Ok(runtime.clone());
        }

        let mut federations = self.federations.write().expect("federations lock poisoned");

        if let Some(runtime) = federations.get(&federation) {
            return Ok(runtime.clone());
        }

        let config = self
            .db
            .begin_read()
            .get(&ClientConfigTable, &federation)
            .context("Federation is not joined")?;

        let runtime = FederationRuntime::build(self, config);

        federations.insert(federation, runtime.clone());

        Ok(runtime)
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

    /// The joined federation's persisted config, without bringing it up.
    pub fn config(&self, federation: FederationId) -> Option<ConsensusConfig> {
        self.db.begin_read().get(&ClientConfigTable, &federation)
    }

    /// The guardians' iroh node ids, read from the persisted config.
    pub fn peer_node_ids(
        &self,
        federation: FederationId,
    ) -> Option<BTreeMap<PeerId, iroh_base::PublicKey>> {
        self.config(federation).map(|config| {
            config
                .peers
                .iter()
                .map(|entry| (*entry.0, entry.1.iroh_pk))
                .collect()
        })
    }

    /// The guardians' broadcast public keys, read from the persisted config.
    pub fn guardian_public_keys(
        &self,
        federation: FederationId,
    ) -> Option<BTreeMap<PeerId, picomint_core::secp256k1::XOnlyPublicKey>> {
        self.config(federation).map(|config| {
            config
                .peers
                .iter()
                .map(|entry| (*entry.0, entry.1.broadcast_pk))
                .collect()
        })
    }

    /// Stream of per-peer guardian reachability, emitting a fresh
    /// `peer -> status` map on every change (current state first). Backed by
    /// the federation's pooled connections, so it reflects the same links
    /// requests travel over; the `Connected` status carries the RTT sampled
    /// at connect. Brings the federation up.
    pub fn connection_status_stream(
        &self,
        federation: FederationId,
    ) -> anyhow::Result<BoxStream<'static, BTreeMap<PeerId, ConnStatus>>> {
        Ok(self.runtime(federation)?.api.connection_status_stream())
    }

    /// The federation's API handle. Brings the federation up.
    pub fn api(&self, federation: FederationId) -> anyhow::Result<FederationApi> {
        Ok(self.runtime(federation)?.api.clone())
    }

    /// The iroh endpoint bound from the seed, shared by every federation's
    /// connections. The gateway daemon serves its public API by accepting on
    /// this endpoint.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Cancel every federation's tasks and wait for them to finish.
    pub async fn shutdown(&self) {
        let federations =
            std::mem::take(&mut *self.federations.write().expect("federations lock poisoned"));

        for runtime in federations.into_values() {
            runtime.tg.shutdown().await;
        }
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

/// The live half of a joined federation: its task group, connection pool,
/// and the module values every operation derives against. Internal — the
/// public surface is [`Client`]'s federation-keyed methods, so nothing
/// outside the crate can hold a runtime across a [`Client::remove`].
pub(crate) struct FederationRuntime {
    pub(crate) api: FederationApi,
    pub(crate) mint: MintClientModule,
    pub(crate) wallet: WalletClientModule,
    pub(crate) ln: LightningClientModule,
    pub(crate) gw: GatewayClientModule,
    tg: TaskGroup,
}

impl FederationRuntime {
    /// Bring up a federation against `config`.
    ///
    /// Not inert: the modules spawn their state-machine executors — resuming
    /// whatever the database already holds — and the background refreshes
    /// commit writes of their own. So this goes last, after the dbtx that
    /// persists `config` and the join's scan results.
    ///
    /// All four modules mount unconditionally: a wallet's gateway module
    /// resumes an empty executor and a gateway's lightning module probes
    /// gateways it never pays through — both cheaper than a flavor concept.
    fn build(client: &Client, config: ConsensusConfig) -> Arc<Self> {
        let federation = config.calculate_federation_id();
        let client_secret = ClientSecret::new(&client.mnemonic, federation);

        let peer_node_ids: BTreeMap<PeerId, iroh_base::PublicKey> = config
            .peers
            .iter()
            .map(|entry| (*entry.0, entry.1.iroh_pk))
            .collect();
        let api: FederationApi = FederationApi::new(client.endpoint.clone(), peer_node_ids);

        let tg = TaskGroup::new();

        let mint_context =
            crate::module::ClientContext::new(api.clone(), client.db.clone(), config.clone());
        let mint = MintClientModule::new(
            federation,
            config.mint.clone(),
            mint_context,
            client_secret.mint_secret(),
            client.fee.as_ref().map_or(0, |fee| fee.ppm),
            &tg,
        );

        let wallet_context =
            crate::module::ClientContext::new(api.clone(), client.db.clone(), config.clone());
        let wallet = WalletClientModule::new(
            config.wallet.clone(),
            wallet_context,
            mint.clone(),
            client_secret.wallet_secret(),
            &tg,
        );

        let ln_context =
            crate::module::ClientContext::new(api.clone(), client.db.clone(), config.clone());
        let ln = LightningClientModule::new(
            federation,
            config.ln.clone(),
            ln_context,
            mint.clone(),
            client_secret.ln_secret(),
            &tg,
        );

        let gw_context =
            crate::module::ClientContext::new(api.clone(), client.db.clone(), config.clone());
        let gw = GatewayClientModule::new(
            federation,
            config.ln.clone(),
            gw_context,
            mint.clone(),
            client_secret.gw_secret(),
            &tg,
        );

        tg.spawn(crate::expiry::refresh(
            api.clone(),
            client.db.clone(),
            federation,
        ));

        // Only when there is a cut to collect: a client that charges nothing
        // has nothing accruing in the account, and a sweep would wake every
        // half minute to read a balance that is always zero.
        if let Some(fee) = client.fee.clone() {
            tg.spawn(crate::fee::sweep(
                client.db.clone(),
                mint.clone(),
                ln.clone(),
                Account::AppFee,
                fee.lnurl,
            ));
        }

        Arc::new(FederationRuntime {
            api,
            mint,
            wallet,
            ln,
            gw,
            tg,
        })
    }
}

/// Cancel-only on drop. Spawned tasks observe the cancellation token at
/// the next await and unwind. [`Client::remove`] and [`Client::shutdown`]
/// wait for them instead.
impl Drop for FederationRuntime {
    fn drop(&mut self) {
        self.tg.cancel();
    }
}
