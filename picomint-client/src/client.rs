use std::collections::BTreeMap;
use std::sync::Arc;

use crate::Endpoint;
use crate::api::FederationApi;
use crate::gw::GatewayClientModule;
use crate::ln::LightningClientModule;
use crate::mint::MintClientModule;
use crate::secret::{ClientSecret, Mnemonic};
use crate::task::TaskGroup;
use crate::wallet::WalletClientModule;
use futures::stream::BoxStream;
use picomint_core::Amount;
use picomint_core::PeerId;
use picomint_core::config::ConsensusConfig;
use picomint_core::config::FederationId;
use picomint_core::core::OperationId;
use picomint_core::invite::InviteCode;
use picomint_eventlog::{EventLogEntry, EventLogId, EventLogger};
use picomint_redb::Database;
use tracing::debug;

/// LN-flavor selection used by the two constructors below.
enum LnChoice {
    Regular,
    Gateway,
}

/// Lightning-module flavor mounted on a client. Regular federation clients
/// use `Regular`, while the gateway daemon mounts `Gateway`. The two flavors
/// are mutually exclusive at the same federation instance.
pub(crate) enum LnFlavor {
    Regular(Arc<LightningClientModule>),
    Gateway(Arc<GatewayClientModule>),
}

/// Main client type
///
/// A handle and API to interacting with a single federation. End user
/// applications that want to support interacting with multiple federations at
/// the same time, will need to instantiate and manage multiple instances of
/// this struct.
///
/// Under the hood it owns service tasks, state machines, database, and other
/// resources. Dropping the last [`Arc<Client>`] cancels all spawned tasks
/// (best-effort, non-blocking); call [`Client::shutdown`] explicitly to wait
/// for them to finish.
pub struct Client {
    config: ConsensusConfig,
    db: Database,
    federation: FederationId,
    logger: EventLogger,
    pub(crate) mint: Arc<MintClientModule>,
    pub(crate) wallet: Arc<WalletClientModule>,
    pub(crate) ln: LnFlavor,
    pub(crate) api: FederationApi,
    tg: TaskGroup,
}

impl Client {
    /// Join a federation for the first time using a regular lightning
    /// flavor. Downloads the federation config via the invite, persists it,
    /// and brings up the client.
    pub fn new(
        endpoint: Endpoint,
        db: Database,
        logger: EventLogger,
        mnemonic: &Mnemonic,
        config: ConsensusConfig,
    ) -> anyhow::Result<Arc<Self>> {
        Self::build(endpoint, db, logger, mnemonic, config, LnChoice::Regular)
    }

    /// Gateway-flavor counterpart of [`Client::new`]. Used by the gateway
    /// daemon, which mounts [`GatewayClientModule`] in place of the regular
    /// lightning module.
    pub fn new_gateway(
        endpoint: Endpoint,
        db: Database,
        logger: EventLogger,
        mnemonic: &Mnemonic,
        config: ConsensusConfig,
    ) -> anyhow::Result<Arc<Self>> {
        Self::build(endpoint, db, logger, mnemonic, config, LnChoice::Gateway)
    }

    fn build(
        endpoint: Endpoint,
        db: Database,
        logger: EventLogger,
        mnemonic: &Mnemonic,
        config: ConsensusConfig,
        ln_choice: LnChoice,
    ) -> anyhow::Result<Arc<Self>> {
        debug!(
            version = %env!("CARGO_PKG_VERSION"),
            "Building picomint client",
        );
        let federation = config.calculate_federation_id();
        let client_secret = ClientSecret::new(mnemonic, federation);

        let peer_node_ids: BTreeMap<PeerId, iroh_base::PublicKey> = config
            .peers
            .iter()
            .map(|(peer, endpoint)| (*peer, endpoint.iroh_pk))
            .collect();
        let api: FederationApi = FederationApi::new(endpoint.clone(), peer_node_ids);

        let tg = TaskGroup::new();

        let mint_context = crate::module::ClientContext::new(
            api.clone(),
            db.clone(),
            logger.clone(),
            config.clone(),
        );
        let mint = Arc::new(MintClientModule::new(
            federation,
            config.mint.clone(),
            mint_context,
            client_secret.mint_secret(),
            &tg,
        )?);

        let wallet_context = crate::module::ClientContext::new(
            api.clone(),
            db.clone(),
            logger.clone(),
            config.clone(),
        );
        let wallet = Arc::new(WalletClientModule::new(
            config.wallet.clone(),
            wallet_context,
            mint.clone(),
            client_secret.wallet_secret(),
            &tg,
        )?);

        let ln = match ln_choice {
            LnChoice::Regular => {
                let ln_context = crate::module::ClientContext::new(
                    api.clone(),
                    db.clone(),
                    logger.clone(),
                    config.clone(),
                );
                LnFlavor::Regular(Arc::new(LightningClientModule::new(
                    federation,
                    config.ln.clone(),
                    ln_context,
                    mint.clone(),
                    client_secret.ln_secret(),
                    &tg,
                )?))
            }
            LnChoice::Gateway => {
                let gw_context = crate::module::ClientContext::new(
                    api.clone(),
                    db.clone(),
                    logger.clone(),
                    config.clone(),
                );
                LnFlavor::Gateway(Arc::new(GatewayClientModule::new(
                    federation,
                    config.ln.clone(),
                    gw_context,
                    mint.clone(),
                    client_secret.gw_secret(),
                    &tg,
                )?))
            }
        };

        let client = Arc::new(Client {
            config,
            db,
            federation,
            logger,
            mint,
            wallet,
            ln,
            api,
            tg,
        });

        client.tg.spawn(Self::refresh_expiry_status(client.clone()));

        Ok(client)
    }

    /// Cancel all spawned tasks and wait for them to finish. No timeout —
    /// blocks until every state machine driver and background task has
    /// observed cancellation and exited cleanly.
    pub async fn shutdown(&self) {
        self.tg.shutdown().await;
    }

    /// Drop every redb table this client owns under its DB prefix.
    /// Intended for shared-database deployments (e.g. the gateway daemon's
    /// per-federation isolated client DBs) where a "leave federation"
    /// operation needs to wipe just one client's data.
    ///
    /// `dbtx` is supplied by the caller so the wipe commits atomically
    /// with whatever else the caller is doing — typically removing
    /// root-level rows scoped to this client by some other key (the
    /// gateway's `ClientConfigTable[federation]`, per-federation event-log
    /// entries, …) and dropping the live `Arc<Client>` from any cache.
    /// The caller is responsible for [`Client::shutdown`] before calling
    /// this so no task is mid-write.
    pub fn wipe(&self, dbtx: &picomint_redb::WriteTx) {
        crate::mint::wipe_tables(dbtx, self.federation);
        crate::wallet::wipe_tables(dbtx, self.federation);
        crate::ln::wipe_tables(dbtx, self.federation);
        crate::gw::wipe_tables(dbtx, self.federation);
        crate::tx::wipe_tables(dbtx, self.federation);
        crate::expiry::wipe_tables(dbtx, self.federation);
    }

    pub fn api(&self) -> &FederationApi {
        &self.api
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

    /// Seed the mint recovery state in `dbtx`. Caller commits this in
    /// the same tx that persists the federation config so "join + start
    /// recovery" is atomic. Returns the operation id the terminal
    /// `RecoveryEvent` will be logged under. The driver is picked up by
    /// the next [`Client::new`] / [`Client::new_gateway`] on the
    /// persisted db. Panics if a recovery is already in progress.
    pub fn init_recovery(dbtx: &picomint_redb::WriteTx, federation: FederationId) -> OperationId {
        crate::mint::init_recovery(dbtx, federation)
    }

    pub fn get_balance(&self) -> Amount {
        self.mint.get_balance(&self.db().begin_read())
    }

    /// Returns a stream that yields the current client balance every time it
    /// changes.
    pub async fn subscribe_balance_changes(&self) -> BoxStream<'static, Amount> {
        let notify = self.mint.balance_notify();
        let initial_balance = self.get_balance();
        let mint = self.mint.clone();
        let db = self.db().clone();

        Box::pin(async_stream::stream! {
            yield initial_balance;
            let mut prev_balance = initial_balance;
            loop {
                let notified = notify.notified();
                let balance = mint.get_balance(&db.begin_read());

                // Deduplicate in case modules cannot always tell if the balance actually changed
                if balance != prev_balance {
                    prev_balance = balance;
                    yield balance;
                }
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

    /// Create an invite code with the api endpoint of the given peer which can
    /// be used to download this client config
    pub fn invite_code(&self, peer: PeerId) -> Option<InviteCode> {
        self.get_peer_node_ids()
            .into_iter()
            .find_map(|(p, node_id)| (peer == p).then_some(node_id))
            .map(|node_id| InviteCode::new(node_id, self.federation()))
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

    pub async fn get_event_log(
        &self,
        pos: EventLogId,
        limit: u64,
    ) -> Vec<(EventLogId, EventLogEntry)> {
        self.logger.get_event_log(&self.db, pos, limit)
    }

    /// Shared [`Notify`] that fires on every commit touching the event log.
    pub fn event_notify(&self) -> Arc<tokio::sync::Notify> {
        self.logger.event_notify(&self.db)
    }

    /// One-shot snapshot of every event currently logged for `operation`,
    /// in insertion order.
    pub fn read_operation_events(&self, operation: OperationId) -> Vec<EventLogEntry> {
        self.logger.read_operation_events(&self.db, operation)
    }

    /// Stream every event belonging to `operation`, starting from the
    /// beginning of the log (existing events first, then live ones).
    pub fn subscribe_operation_events(
        &self,
        operation: OperationId,
    ) -> BoxStream<'static, EventLogEntry> {
        Box::pin(self.logger.subscribe_operation_events(
            self.db.clone(),
            self.event_notify(),
            operation,
        ))
    }
}

/// Cancel-only on drop. Spawned tasks observe the cancellation token at
/// the next await and unwind. Callers wanting to wait for tasks to
/// complete should `client.shutdown().await` first.
impl Drop for Client {
    fn drop(&mut self) {
        self.tg.cancel();
    }
}
