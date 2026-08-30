use std::collections::{BTreeMap, HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use flutter_rust_bridge::frb;
use futures::StreamExt;
use futures::stream::{self, BoxStream};
use iroh::Endpoint;
use iroh::endpoint::presets::N0;
use iroh_mdns_address_lookup::MdnsAddressLookup;
use picomint_client::{Account, Client, Mnemonic, OperationId};
use picomint_core::bitcoin::hashes::sha256;
use picomint_core::config::FederationId;
use picomint_eventlog::{EventLogId, EventLogger};
use picomint_redb::Database;
use tokio::sync::{Mutex, Notify, RwLock};

use crate::client::PicoClient;
use crate::db::{
    CONTACT, ClientConfig, EventLog, EventLogByOperation, OperationFiat, RootEntropy,
    SelectedCurrency,
};
use crate::events::{
    Notification, OperationSummary, PaymentEvent, is_summary_trigger, parse_notification,
    parse_payment_event, parse_summary,
};
use crate::exchange::{ExchangeRateCache, FRESHNESS, btc_price};
use crate::frb_generated::StreamSink;
use crate::lnurl::LnurlWrapper;
use crate::{DatabaseWrapper, InviteCodeWrapper, MnemonicWrapper};

#[frb(opaque)]
pub struct PicoClientFactory {
    db: Database,
    /// Daemon-wide event log over the app's `EVENT_LOG` tables. Cloned into
    /// every `Client::new` so all federations append to one ordered log.
    logger: EventLogger,
    mnemonic: Mnemonic,
    /// Single iroh endpoint shared across all per-federation clients.
    /// Address grinding is the slowest part of bringup so we bind once
    /// at factory construction and reuse for every `Client::new`.
    endpoint: Endpoint,
    /// All warm clients, keyed by `(FederationId, Account)` — one entry per
    /// account, three per joined federation, all three sharing that
    /// federation's single `Arc<Client>`. Constructed at startup from
    /// `ClientConfig`; `join` inserts a federation's whole row of
    /// accounts, `leave` removes it. The key order is federation-major and
    /// account-minor, which is the order the home pager swipes in.
    ///
    /// Re-joining a previously-left federation reuses the same keys —
    /// `Client::wipe` clears the federation's prefixed tables on leave, so
    /// the second join sees a clean state.
    clients: Arc<RwLock<BTreeMap<(FederationId, Account), PicoClient>>>,
    /// Wakes anyone iterating the client set when membership changes.
    /// `notify_waiters` is fire-and-forget; subscribers re-snapshot the
    /// map after waking.
    set_changed: Arc<Notify>,
    /// Single exchange-rate cache shared by every client (the BTC price is
    /// global, not per-federation) and by the fiat-snapshot recorder. One
    /// fetch warms it for all consumers.
    exchange_rate_cache: ExchangeRateCache,
}

#[frb(opaque)]
pub struct PicoContact {
    lnurl: LnurlWrapper,
    name: String,
}

fn contains(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

impl PicoContact {
    #[frb(sync, getter)]
    pub fn name(&self) -> String {
        self.name.clone()
    }

    #[frb(sync, getter)]
    pub fn lnurl(&self) -> LnurlWrapper {
        LnurlWrapper(self.lnurl.0.clone())
    }

    #[frb(sync)]
    pub fn match_query(&self, query: &str) -> bool {
        contains(&self.name, query) || contains(&self.lnurl.0, query)
    }
}

impl PicoClientFactory {
    #[frb]
    pub async fn init(db: &DatabaseWrapper, mnemonic: &MnemonicWrapper) -> Result<Self, String> {
        let dbtx = db.0.begin_write();

        dbtx.insert(&RootEntropy, &(), &mnemonic.0.to_entropy().to_vec());

        dbtx.commit();

        let endpoint = bind_endpoint().await.map_err(|e| e.to_string())?;

        Self::assemble(db.0.clone(), mnemonic.0.clone(), endpoint).await
    }

    #[frb]
    pub async fn try_load(db: &DatabaseWrapper) -> Option<Self> {
        let entropy = db.0.begin_read().get(&RootEntropy, &())?;

        let mnemonic = Mnemonic::from_entropy(&entropy).ok()?;

        let endpoint = bind_endpoint().await.ok()?;

        Self::assemble(db.0.clone(), mnemonic, endpoint).await.ok()
    }

    /// Build the factory and warm every persisted federation into a
    /// ready-to-use `PicoClient`. Each `Client::new` here re-runs the
    /// per-federation handshake; doing them in parallel keeps cold
    /// startup time bounded by the slowest peer rather than their sum.
    async fn assemble(
        db: Database,
        mnemonic: Mnemonic,
        endpoint: Endpoint,
    ) -> Result<Self, String> {
        let entries: Vec<(FederationId, picomint_core::config::ConsensusConfig)> =
            db.begin_read().iter(&ClientConfig, |it| it.collect());

        let logger = EventLogger::new(EventLog, EventLogByOperation);

        let exchange_rate_cache: ExchangeRateCache = Arc::new(Mutex::new(None));

        let mut warmed: BTreeMap<(FederationId, Account), PicoClient> = BTreeMap::new();
        for (fed_id, config) in entries {
            let client = Client::new(
                endpoint.clone(),
                db.clone(),
                logger.clone(),
                &mnemonic,
                config,
                Some(crate::payout::fee_config()),
            );

            warmed.extend(build_accounts(
                client,
                fed_id,
                db.clone(),
                exchange_rate_cache.clone(),
            ));
        }

        let clients = Arc::new(RwLock::new(warmed));

        Ok(Self {
            db,
            logger,
            mnemonic,
            endpoint,
            clients,
            set_changed: Arc::new(Notify::new()),
            exchange_rate_cache,
        })
    }

    #[frb]
    pub async fn seed_phrase(&self) -> Vec<String> {
        self.mnemonic.words().map(|s| s.to_string()).collect()
    }

    /// Snapshot of every warm client. Cheap (`PicoClient: Clone`) — the
    /// inner `Arc<Client>` is shared, so callers all see the same
    /// connection state.
    #[frb]
    pub async fn clients(&self) -> Vec<PicoClient> {
        self.clients.read().await.values().cloned().collect()
    }

    /// Look up a federation's [`Account::PRIMARY`] client. `None` if the user
    /// isn't joined to it, which is how the ecash drawer tells a bundle from
    /// a mint the wallet doesn't have.
    ///
    /// Primary because a caller reaching for this holds a federation id and
    /// nothing more — an ecash bundle names no account. It is the fallback,
    /// not the usual path: the drawer receives into the account on screen
    /// when the bundle belongs to its federation, and only asks here when it
    /// doesn't.
    #[frb]
    pub async fn client(&self, federation_id: &str) -> Option<PicoClient> {
        let id = FederationId::from_str(federation_id).ok()?;
        self.clients
            .read()
            .await
            .get(&(id, Account::PRIMARY))
            .cloned()
    }

    /// Adds a federation, rebuilding whatever this seed already owns there.
    ///
    /// One path, whether or not the seed has been here before: `join` scans
    /// every account before anything is opened or written, and a seed that
    /// never held anything scans to nothing. There is no question to put to
    /// the user and so nothing for them to get wrong — answering "new mint"
    /// for a federation this seed has used would write counter zero over an
    /// account that has issued and strand every note behind the nonces it
    /// re-derives.
    ///
    /// Returns only once the notes are back, so there is no progress to
    /// report and no half-joined federation to detect on the next launch.
    #[frb]
    pub async fn join(&self, invite: &InviteCodeWrapper) -> Result<PicoClient, String> {
        // Rejected before the scan rather than after it. The invite code
        // commits to the federation id — `join` below refuses a config that
        // computes to any other — so a duplicate is knowable up front, and
        // there is no reason to make the user wait out four account scans to
        // be told what the code alone already said.
        if self
            .clients
            .read()
            .await
            .contains_key(&(invite.0.federation, Account::PRIMARY))
        {
            return Err("This mint is already added".to_string());
        }

        // Reads nothing and writes nothing locally, so a failure here leaves
        // the wallet exactly as it was — the federation stays unjoined.
        let join = picomint_client::join(&self.endpoint, &self.mnemonic, &invite.0)
            .await
            .map_err(|e| e.to_string())?;

        let config = join.config().clone();

        let federation_id = config.calculate_federation_id();

        let dbtx = self.db.begin_write();

        // The check above is only as fresh as the read that took it, and the
        // scan is a long await for a second join of the same invite to land
        // in; this one is atomic with the write, so it is the authority.
        // Rejecting rather than handing back what is already there matters
        // most here: the counter marks below describe how far this seed's
        // counter space was walked, and writing them over a live federation
        // would rewind it. Insert reports whatever it displaced, and
        // returning drops the dbtx uncommitted — which aborts it, so the row
        // we overwrote is left exactly as it was.
        if dbtx
            .insert(&ClientConfig, &federation_id, &config)
            .is_some()
        {
            return Err("This mint is already added".to_string());
        }

        // Counter marks and restored notes, riding the same commit as the
        // join itself: either the federation is joined with every counter in
        // place and its balance already there, or it isn't joined at all.
        join.commit(&dbtx);

        dbtx.commit();

        let client = Client::new(
            self.endpoint.clone(),
            self.db.clone(),
            self.logger.clone(),
            &self.mnemonic,
            config,
            Some(crate::payout::fee_config()),
        );

        let accounts = build_accounts(
            client,
            federation_id,
            self.db.clone(),
            self.exchange_rate_cache.clone(),
        );

        // Primary is what a caller that just joined gets handed: it is the
        // account the pager lands on, and the only one a screen holding a
        // single client can mean.
        let pico = accounts[&(federation_id, Account::PRIMARY)].clone();

        self.clients.write().await.extend(accounts);
        self.set_changed.notify_waiters();

        Ok(pico)
    }

    #[frb]
    pub async fn set_currency(&self, currency_code: &str) {
        let dbtx = self.db.begin_write();

        dbtx.insert(&SelectedCurrency, &(), &currency_code.to_string());

        dbtx.commit();
    }

    #[frb]
    pub async fn get_currency(&self) -> String {
        self.currency().await
    }

    async fn currency(&self) -> String {
        self.db
            .begin_read()
            .get(&SelectedCurrency, &())
            .unwrap_or_else(|| "USD".to_string())
    }

    /// Drop a federation: shut down the client, wipe its per-federation
    /// prefixed tables, then drop the config row. Wipe + remove
    /// share a single write tx so a crash mid-leave can never leave
    /// orphan client state behind a missing config row. Re-joining the
    /// same federation later starts from a fresh ledger.
    #[frb]
    pub async fn leave(&self, federation_id: &str) -> Result<(), String> {
        let fed_id = FederationId::from_str(federation_id).map_err(|e| e.to_string())?;

        // Every account goes at once — accounts are a split of one federation
        // client, not something a user joins or leaves individually. The
        // shutdown and wipe below run once for the client all three shared.
        let mut guard = self.clients.write().await;
        let removed: Vec<PicoClient> = Account::USER_ACCOUNTS
            .into_iter()
            .filter_map(|account| guard.remove(&(fed_id, account)))
            .collect();
        drop(guard);

        let Some(client) = removed.into_iter().next() else {
            return Ok(());
        };

        client.client.shutdown().await;

        let dbtx = self.db.begin_write();
        client.client.wipe(&dbtx);
        dbtx.remove(&ClientConfig, &fed_id);
        dbtx.commit();

        self.set_changed.notify_waiters();
        Ok(())
    }

    /// Live snapshot of every warm client; re-emits on every set change
    /// (`join`/`leave`). Subscribers re-render passively
    /// instead of re-fetching `clients()` after each navigation pop.
    #[frb]
    pub async fn subscribe_clients(&self, sink: StreamSink<Vec<PicoClient>>) {
        loop {
            let snapshot: Vec<PicoClient> = self.clients.read().await.values().cloned().collect();
            let set_changed = self.set_changed.notified();
            tokio::pin!(set_changed);
            if sink.add(snapshot).is_err() {
                return;
            }
            set_changed.await;
        }
    }

    /// Aggregated balance across every warm client, in sats. Re-emits on
    /// any per-client balance change AND on client-set changes
    /// (`join`/`leave`). The totals map survives rebuilds so a
    /// join/leave doesn't reset the running sum to zero.
    #[frb]
    pub async fn subscribe_global_balance(&self, sink: StreamSink<i64>) {
        let mut totals: HashMap<(FederationId, Account), i64> = HashMap::new();

        loop {
            // Snapshot the live client set; build a tagged stream per
            // client so we can attribute incoming balances back to an
            // account and discard departed clients on the next rebuild.
            let snapshot: Vec<((FederationId, Account), PicoClient)> = self
                .clients
                .read()
                .await
                .iter()
                .map(|(k, v)| (*k, v.clone()))
                .collect();

            let alive: HashSet<(FederationId, Account)> =
                snapshot.iter().map(|(k, _)| *k).collect();
            totals.retain(|key, _| alive.contains(key));

            let mut tagged: Vec<BoxStream<'static, ((FederationId, Account), i64)>> =
                Vec::with_capacity(snapshot.len());
            for (key, client) in snapshot {
                let stream = client
                    .client
                    .subscribe_balance_changes(client.account)
                    .map(move |amt| (key, (amt.msat / 1000) as i64));
                tagged.push(stream.boxed());
            }
            let mut merged = stream::select_all(tagged);

            // Re-arm the set-change notifier *before* emitting the
            // initial sum, so a join/leave landing between the snapshot
            // and the await still wakes us.
            let set_changed = self.set_changed.notified();
            tokio::pin!(set_changed);

            if sink.add(totals.values().sum()).is_err() {
                return;
            }

            loop {
                tokio::select! {
                    Some((key, balance)) = merged.next() => {
                        totals.insert(key, balance);
                        if sink.add(totals.values().sum()).is_err() {
                            return;
                        }
                    }
                    _ = &mut set_changed => break,
                }
            }
        }
    }

    /// One-shot list of every operation across every federation in
    /// chronological order (oldest first — Dart reverses for display).
    /// Cards rendered from this snapshot stay static; live status is
    /// reachable only by opening the per-op drawer.
    #[frb]
    pub async fn list_operations(&self) -> Vec<OperationSummary> {
        let names = self.federation_names_snapshot().await;
        let mut position = EventLogId::LOG_START;
        let mut summaries: Vec<OperationSummary> = Vec::new();

        loop {
            let batch = self.logger.get_event_log(&self.db, position, 1000);

            for entry in &batch {
                let fiat = self.db.begin_read().get(&OperationFiat, &entry.1.operation);
                if let Some(summary) = parse_summary(&entry.1, &names, fiat) {
                    summaries.push(summary);
                }
            }

            position = position.saturating_add(batch.len() as u64);

            if batch.len() < 1000 {
                break;
            }
        }

        summaries
    }

    /// Live ordered list of operation summaries (newest first) across
    /// every federation. Emits once after the historical replay
    /// completes, then re-emits whenever a new trigger event lands.
    /// Follow-up events that only change live status do not re-emit —
    /// those reach the UI through `subscribe_payment_events` when the
    /// user opens the drawer.
    #[frb]
    pub async fn subscribe_recent_operations(&self, sink: StreamSink<Vec<OperationSummary>>) {
        // Phase 1: drain history into the full summaries vector. No emits.
        let mut summaries: Vec<OperationSummary> = Vec::new();
        let mut position = EventLogId::LOG_START;
        let names = self.federation_names_snapshot().await;

        loop {
            let batch = self.logger.get_event_log(&self.db, position, 1000);

            for entry in &batch {
                let fiat = self.db.begin_read().get(&OperationFiat, &entry.1.operation);
                if let Some(summary) = parse_summary(&entry.1, &names, fiat) {
                    summaries.push(summary);
                }
            }

            position = position.saturating_add(batch.len() as u64);

            if batch.len() < 1000 {
                break;
            }
        }

        summaries = summaries.into_iter().rev().take(3).rev().collect();

        if sink.add(summaries.clone()).is_err() {
            return;
        }

        // Phase 2: tail live events. Re-snapshot names per batch so a
        // newly-joined federation's name lands on its own first event.
        let notify: Arc<Notify> = self.logger.event_notify(&self.db);

        loop {
            let notified = notify.notified();

            let batch = self.logger.get_event_log(&self.db, position, 1000);
            let names = self.federation_names_snapshot().await;

            for entry in &batch {
                // Price each new payment as we observe it, so the summary we
                // emit already carries its fiat value rather than gaining it
                // only on a later restart.
                let fiat = if is_summary_trigger(&entry.1) {
                    snapshot_fiat(&self.db, &self.exchange_rate_cache, &entry.1.operation)
                } else {
                    None
                };
                if let Some(summary) = parse_summary(&entry.1, &names, fiat) {
                    summaries.push(summary);
                }
            }

            if sink.add(summaries.clone()).is_err() {
                return;
            }

            position = position.saturating_add(batch.len() as u64);

            if batch.len() < 1000 {
                notified.await;
            }
        }
    }

    /// Snapshot of currently-warm federation ids → names. Used to
    /// resolve `OperationSummary.federation_name` at parse time. The three
    /// accounts of a federation all carry its name, so they collapse onto one
    /// entry here.
    async fn federation_names_snapshot(&self) -> BTreeMap<FederationId, String> {
        self.clients
            .read()
            .await
            .iter()
            .map(|((id, _), c)| (*id, c.federation_name.clone()))
            .collect()
    }

    /// Live tail of every picomint event for a single operation, parsed
    /// into the rich [`PaymentEvent`] enum for the details drawer timeline.
    /// Replays existing events first (oldest → newest) then yields new
    /// ones as they're committed. Silently exits if `operation_id` doesn't
    /// parse as a valid sha256 hash. Operation ids are globally unique so
    /// no federation context is required — reads the daemon-wide eventlog
    /// directly.
    #[frb]
    pub async fn subscribe_payment_events(
        &self,
        operation_id: String,
        sink: StreamSink<PaymentEvent>,
    ) {
        let Ok(hash) = sha256::Hash::from_str(&operation_id) else {
            return;
        };
        let op = OperationId(hash);

        let notify = self.logger.event_notify(&self.db);
        let mut stream = self
            .logger
            .subscribe_operation_events(self.db.clone(), notify, op)
            .boxed();

        while let Some(entry) = stream.next().await {
            let Some(event) = parse_payment_event(&entry) else {
                continue;
            };
            if sink.add(event).is_err() {
                break;
            }
        }
    }

    /// Toast/haptic stream — fires per matching event committed after
    /// the historical replay. Spans every federation, since the picomint
    /// eventlog is daemon-wide.
    #[frb]
    pub async fn subscribe_notifications(&self, sink: StreamSink<Notification>) {
        // Phase 1: drain history to find the live position. No
        // notifications fire — these are old events.
        let mut position = EventLogId::LOG_START;

        loop {
            let batch = self.logger.get_event_log(&self.db, position, 1000);

            position = position.saturating_add(batch.len() as u64);

            if batch.len() < 1000 {
                break;
            }
        }

        // Phase 2: tail live events; every match fires a notification.
        let notify: Arc<Notify> = self.logger.event_notify(&self.db);

        loop {
            let notified = notify.notified();

            let batch = self.logger.get_event_log(&self.db, position, 1000);

            for entry in &batch {
                if let Some(notification) = parse_notification(&entry.1) {
                    if sink.add(notification).is_err() {
                        return;
                    }
                }
            }

            position = position.saturating_add(batch.len() as u64);

            if batch.len() < 1000 {
                notified.await;
            }
        }
    }

    #[frb]
    pub async fn save_contact(&self, lnurl: &LnurlWrapper, name: &str) {
        let dbtx = self.db.begin_write();

        dbtx.insert(&CONTACT, &lnurl.0, &name.to_string());

        dbtx.commit();
    }

    #[frb]
    pub async fn get_contact_name(&self, lnurl: &LnurlWrapper) -> Option<String> {
        self.db.begin_read().get(&CONTACT, &lnurl.0)
    }

    #[frb]
    pub async fn list_contacts(&self) -> Vec<PicoContact> {
        let mut contacts: Vec<_> = self.db.begin_read().iter(&CONTACT, |it| {
            it.map(|(lnurl, name)| PicoContact {
                lnurl: LnurlWrapper(lnurl),
                name,
            })
            .collect()
        });

        contacts.sort_by_key(|c| c.name.to_lowercase());

        contacts
    }

    #[frb]
    pub async fn delete_contact(&self, lnurl: &LnurlWrapper) {
        let dbtx = self.db.begin_write();

        dbtx.remove(&CONTACT, &lnurl.0);

        dbtx.commit();
    }
}

/// One `PicoClient` per account, all sharing `client` — the federation's
/// whole row of the map, ready to be `extend`ed into it. Keyed the way the map
/// is, so a caller never has to pair a client back up with its key.
fn build_accounts(
    client: Arc<Client>,
    federation_id: FederationId,
    db: Database,
    exchange_rate_cache: ExchangeRateCache,
) -> BTreeMap<(FederationId, Account), PicoClient> {
    let federation_name = client.config().name.clone();

    Account::USER_ACCOUNTS
        .into_iter()
        .map(|account| {
            (
                (federation_id, account),
                PicoClient {
                    client: client.clone(),
                    federation_id,
                    account,
                    federation_name: federation_name.clone(),
                    db: db.clone(),
                    exchange_rate_cache: exchange_rate_cache.clone(),
                },
            )
        })
        .collect()
}

/// Snapshot the live exchange rate against a freshly-observed trigger event,
/// returning the stored `(currency, rate)`. Idempotent write-if-absent: returns
/// an existing snapshot untouched, otherwise reads the selected currency and
/// the cached rate (never fetches) and persists it. `None` — and no write —
/// when no fresh rate is cached or the feed lacks this currency's pair, so the
/// operation falls back to sats. Call only for live trigger events; historical
/// ones predate the session and stay unpriced.
fn snapshot_fiat(
    db: &Database,
    cache: &ExchangeRateCache,
    op: &OperationId,
) -> Option<(String, f64)> {
    if let Some(existing) = db.begin_read().get(&OperationFiat, op) {
        return Some(existing);
    }

    let currency = db
        .begin_read()
        .get(&SelectedCurrency, &())
        .unwrap_or_else(|| "USD".to_string());

    // Derive the selected currency's rate from the cached map without hitting
    // the network; bail (sats fallback) when nothing fresh is cached or the
    // feed lacks this currency's pair.
    let rate = {
        let guard = cache.try_lock().ok()?;
        let (prices, timestamp) = guard.as_ref()?;
        if timestamp.elapsed() >= FRESHNESS {
            return None;
        }
        btc_price(prices, &currency)?
    };

    let dbtx = db.begin_write();
    dbtx.insert(&OperationFiat, op, &(currency.clone(), rate));
    dbtx.commit();

    Some((currency, rate))
}

async fn bind_endpoint() -> anyhow::Result<Endpoint> {
    Endpoint::builder(N0)
        .address_lookup(MdnsAddressLookup::builder())
        .bind()
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))
}
