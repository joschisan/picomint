use std::collections::BTreeMap;
use std::sync::Arc;

use bitcoin::Amount as BtcAmount;
use flutter_rust_bridge::frb;
use futures::StreamExt;
use picomint_client::{Account, Client, ConnStatus};
use picomint_core::Amount;
use picomint_core::PeerId;
use picomint_core::config::FederationId;
use picomint_core::ln::gateway::{GatewayInfo, GatewayPk};
use picomint_redb::Database;

use crate::db::SelectedCurrency;
use crate::exchange::{ExchangeRateCache, FRESHNESS, btc_price, fetch_exchange_rates};
use crate::frb_generated::StreamSink;
use crate::{BitcoinAddressWrapper, Bolt11InvoiceWrapper, ECashWrapper, InviteCodeWrapper};

/// Holds a caller-selected gateway plus its routing info, returned by
/// [`PicoClient::ln_select_gateway`] and handed back to
/// [`PicoClient::ln_send`] so the fee we previewed is the fee we pay.
/// Opaque on purpose — Dart only needs the two fee getters.
#[frb(opaque)]
#[derive(Clone)]
pub struct GatewayInfoWrapper {
    pub(crate) gateway_pk: GatewayPk,
    pub(crate) gateway_info: GatewayInfo,
}

impl GatewayInfoWrapper {
    /// Exact fee (sats) for paying this invoice through this gateway. One
    /// flat price however the payment settles — routed over Lightning or
    /// swapped internally by the invoice's own issuer — so nothing about the
    /// invoice changes what it costs.
    #[frb(sync)]
    pub fn gateway_fee_for_invoice(&self, invoice: &Bolt11InvoiceWrapper) -> i64 {
        self.gateway_fee_for_amount((invoice.0.amount_milli_satoshis().unwrap_or(0) / 1000) as i64)
    }

    /// Exact fee (sats) for paying `amount_sats` through this gateway — the
    /// same flat price an invoice for the amount will be quoted.
    #[frb(sync)]
    pub fn gateway_fee_for_amount(&self, amount_sats: i64) -> i64 {
        let msats = (amount_sats as u64).saturating_mul(1000);

        (self.gateway_info.send_fee.fee(msats).msat / 1000) as i64
    }

    /// Fee (sats) the gateway deducts from a `amount_sats` incoming
    /// payment. The recipient ultimately ends up with `amount - fee`.
    #[frb(sync)]
    pub fn gateway_fee_for_receive_amount(&self, amount_sats: i64) -> i64 {
        let msats = (amount_sats as u64).saturating_mul(1000);
        (self.gateway_info.receive_fee.fee(msats).msat / 1000) as i64
    }
}

#[frb(opaque)]
#[derive(Clone)]
pub struct PicoClient {
    pub(crate) client: Arc<Client>,
    pub(crate) federation_id: FederationId,
    /// Which of the federation client's three balances this handle spends
    /// from. The `Arc<Client>` above is shared by all three — an account is a
    /// client-side split of the derivation tree, not a separate connection —
    /// so the factory holds one `PicoClient` per (federation, account) pair
    /// and every money method below passes this down.
    pub(crate) account: Account,
    /// Cached at construction so the factory can resolve names for
    /// `OperationSummary` synchronously while iterating the event log.
    pub(crate) federation_name: String,
    /// App database handle, so the selected currency is read fresh from
    /// `SelectedCurrency` on every conversion — the db is the single source
    /// of truth, never mirrored in memory where it could drift after a switch.
    pub(crate) db: Database,
    pub(crate) exchange_rate_cache: ExchangeRateCache,
}

impl PicoClient {
    #[frb]
    pub async fn federation_name(&self) -> Option<String> {
        Some(self.federation_name.clone())
    }

    #[frb(sync)]
    pub fn federation_id(&self) -> String {
        self.federation_id.to_string()
    }

    /// This handle's account, as the name the UI shows beneath the federation
    /// — `Primary`, `Secondary` or `Tertiary`. Sync because it is fixed at
    /// construction; a federation id alone no longer identifies a client, so
    /// the two together are what callers key by.
    #[frb(sync)]
    pub fn account_name(&self) -> String {
        self.account.to_string()
    }

    #[frb(sync)]
    pub fn currency_code(&self) -> String {
        self.db
            .begin_read()
            .get(&SelectedCurrency, &())
            .unwrap_or_else(|| "USD".to_string())
    }

    #[frb]
    pub async fn shutdown(&self) {
        self.client.shutdown().await;
    }

    /// Warm the exchange-rate cache, resolving only once a rate is actually
    /// stored (or the fetch fails). Awaiting it lets callers repaint
    /// fiat-dependent UI the moment a rate becomes available; a fresh cache
    /// short-circuits without a network hit.
    #[frb]
    pub async fn prefetch_exchange_rates(&self) {
        let _ = fetch_exchange_rates(self.exchange_rate_cache.clone()).await;
    }

    /// Converts a fiat amount in `currency_code` to sats. The caller supplies
    /// the currency: the home screen reads it live via [`currency_code`], while
    /// the send/receive amount flow snapshots it once on entry (the user can't
    /// change currency mid-flow), so this does no db read of its own.
    #[frb]
    pub async fn fiat_to_sats(
        &self,
        amount_fiat: f64,
        currency_code: String,
    ) -> Result<i64, String> {
        fetch_exchange_rates(self.exchange_rate_cache.clone()).await?;

        let guard = self.exchange_rate_cache.lock().await;
        let (prices, _) = guard.as_ref().ok_or("No exchange rate cached")?;
        let rate = btc_price(prices, &currency_code).ok_or("Currency not supported")?;

        Ok(((amount_fiat / rate) * 100_000_000.0).round() as i64)
    }

    /// Converts `amount_sats` to `currency_code` using the cached exchange
    /// rate, without triggering a network fetch. Returns `None` when no fresh
    /// rate is cached, so callers can omit the fiat row rather than block on
    /// the network. Currency is caller-supplied — see [`fiat_to_sats`].
    #[frb(sync)]
    pub fn sats_to_fiat(&self, amount_sats: i64, currency_code: String) -> Option<f64> {
        let guard = self.exchange_rate_cache.try_lock().ok()?;
        let (prices, timestamp) = guard.as_ref()?;
        if timestamp.elapsed() >= FRESHNESS {
            return None;
        }
        let rate = btc_price(prices, &currency_code)?;
        Some((amount_sats as f64 / 100_000_000.0) * rate)
    }

    #[frb]
    pub async fn subscribe_balance(&self, sink: StreamSink<i64>) {
        let mut stream = self.client.subscribe_balance_changes(self.account);

        while let Some(amount) = stream.next().await {
            if sink.add((amount.msat / 1000) as i64).is_err() {
                break;
            }
        }
    }

    /// Live per-guardian reachability, one entry per guardian in
    /// `config().peers` (PeerId) order: `(name, rtt_ms)` where `rtt_ms` is
    /// `Some(round-trip millis)` while connected and `None` while
    /// disconnected. Sourced from the client's `connection_status_stream`,
    /// which is backed by the same kept-alive connections requests travel
    /// over and emits the current snapshot first — so a freshly-opened
    /// screen never shows a cold-start flicker. Multiple subscribers (home
    /// ring + connection-status screen) each get their own cheap view of
    /// the shared connections; subscribing starts no new polling.
    #[frb]
    pub async fn subscribe_connection_status(&self, sink: StreamSink<Vec<(String, Option<f64>)>>) {
        // Guardian names keyed by PeerId so every emission renders all
        // guardians (even before their first status lands) in a stable order.
        let names: BTreeMap<PeerId, String> = self
            .client
            .config()
            .peers
            .iter()
            .map(|(id, peer)| (*id, peer.name.clone()))
            .collect();

        let mut stream = self.client.connection_status_stream();

        while let Some(status_map) = stream.next().await {
            let statuses: Vec<(String, Option<f64>)> = names
                .iter()
                .map(|(peer, name)| {
                    let rtt_ms = match status_map.get(peer) {
                        Some(ConnStatus::Connected(rtt)) => Some(rtt.as_secs_f64() * 1000.0),
                        _ => None,
                    };
                    (name.clone(), rtt_ms)
                })
                .collect();

            if sink.add(statuses).is_err() {
                break;
            }
        }
    }

    /// Federation-expiry metadata. Picomint has no MetaService yet, so
    /// always `None` — UI screens that key off this stay dormant.
    #[frb]
    pub async fn expiration_date(&self) -> Option<i64> {
        None
    }

    /// Successor-federation invite. Same story as `expiration_date` —
    /// stubbed until picomint exposes a metadata channel.
    #[frb]
    pub async fn expiration_successor(&self) -> Option<InviteCodeWrapper> {
        None
    }

    #[frb]
    pub async fn ecash_send(&self, amount_sat: i64) -> Result<ECashWrapper, String> {
        self.client
            .mint()
            .send(self.account, Amount::from_sat(amount_sat as u64))
            .await
            .map(ECashWrapper)
            .map_err(|e| e.to_string())
    }

    /// Everything this account holds, as one note bundle.
    ///
    /// Not `ecash_send` with the balance as its amount: `send` rounds up to a
    /// whole denomination and takes its fast path only when the account's
    /// notes sum to that amount exactly, which a figure in whole sats
    /// generally isn't — denominations are powers of two millisats, and the
    /// balance the UI shows is millisats floored to sats besides. Missing the
    /// fast path drops the send onto the path that builds a real transaction,
    /// which covers its fees out of the account being emptied, so asking for
    /// everything fails on a balance the user is looking at. See
    /// [`Self::transfer_to_primary`], which sends the same way for the same
    /// reason.
    ///
    /// Infallible: `None` is an account holding no notes, which is the empty
    /// wallet the caller shouldn't have offered this on.
    #[frb]
    pub async fn ecash_send_max(&self) -> Option<ECashWrapper> {
        self.client.mint().send_max(self.account).map(ECashWrapper)
    }

    #[frb]
    pub async fn ecash_receive(&self, ecash: &ECashWrapper) -> Result<(), String> {
        self.client
            .mint()
            .receive(self.account, &ecash.0)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Move everything this account holds into its federation's primary
    /// account, as an ordinary ecash payment between the two. The receive
    /// leg reissues the notes to primary's own nonces, so a later restore
    /// from the seed finds them where they now are rather than where they
    /// were.
    ///
    /// Sends by `send_max` rather than by naming the balance in sats. `send`
    /// rounds its amount up to a whole denomination and takes its fast path
    /// only when the account's notes sum to that amount exactly — and a
    /// figure rounded to whole sats generally isn't one they can make, since
    /// denominations are powers of two millisats. Missing it drops the send
    /// onto the path that builds a real transaction, which has to cover its
    /// own fees out of the account being emptied, so asking for everything
    /// fails with `InsufficientBalance` on a balance the user is looking at.
    ///
    /// Both legs run here rather than either side of the bridge: the send
    /// hands the notes back by value, so until the receive lands they are
    /// held nowhere else, and a receive that fails puts them back into the
    /// account they came from.
    #[frb]
    pub async fn transfer_to_primary(&self) -> Result<(), String> {
        let Some(ecash) = self.client.mint().send_max(self.account) else {
            return Ok(());
        };

        if let Err(error) = self.client.mint().receive(Account::PRIMARY, &ecash) {
            // The guard row a receive takes is only committed on success, so
            // this second attempt isn't refused as a repeat of the first.
            self.client.mint().receive(self.account, &ecash).ok();

            return Err(error.to_string());
        }

        Ok(())
    }

    #[frb]
    pub async fn ln_receive(
        &self,
        gateway: &GatewayInfoWrapper,
        amount_sat: i64,
    ) -> Result<String, String> {
        let invoice = self
            .client
            .ln()
            .receive(
                self.account,
                gateway.gateway_pk,
                gateway.gateway_info.clone(),
                Amount::from_sat(amount_sat as u64),
            )
            .await
            .map_err(|e| e.to_string())?;

        Ok(invoice.to_string())
    }

    /// Pre-select an online gateway. Any will do for any payment: a gateway
    /// charges the same fee however a payment settles, so there is nothing
    /// about an invoice to select one by.
    #[frb]
    pub async fn ln_select_gateway(&self) -> Result<GatewayInfoWrapper, String> {
        let (gateway_pk, gateway_info) = self
            .client
            .ln()
            .select_gateway()
            .map_err(|e| e.to_string())?;

        Ok(GatewayInfoWrapper {
            gateway_pk,
            gateway_info,
        })
    }

    #[frb]
    pub async fn ln_send(
        &self,
        gateway: &GatewayInfoWrapper,
        invoice: &Bolt11InvoiceWrapper,
    ) -> Result<String, String> {
        self.client
            .ln()
            .send(
                self.account,
                gateway.gateway_pk,
                gateway.gateway_info.clone(),
                invoice.0.clone(),
            )
            .await
            .map(|op| op.to_string())
            .map_err(|e| e.to_string())
    }

    /// The largest whole-sat payment this account can make through `gateway`:
    /// what its notes deliver when spent in full, less the gateway's flat
    /// fee, the transaction fee and the app's cut. Needs no invoice — the
    /// gateway's fee is the same however a payment settles, so the figure
    /// holds for whatever invoice is later resolved for it.
    ///
    /// Picomint's figure, not ours: a max send funds from every note and
    /// mints no change at exactly this amount, so the sizing has to live
    /// where the spending does.
    #[frb]
    pub async fn ln_max_amount(&self, gateway: &GatewayInfoWrapper) -> i64 {
        let amount = self
            .client
            .ln()
            .send_max_amount(self.account, &gateway.gateway_info);

        (amount.msat / 1000) as i64
    }

    /// Empties the account to `lnurl` through `gateway`: picomint resolves
    /// the one invoice it pays, sized fresh by the same code that spends it,
    /// so every note goes in and no change comes back. The figure
    /// [`Self::ln_max_amount`] previewed through this gateway is the figure
    /// paid, short of the balance moving in between — which moves the
    /// payment with it.
    #[frb]
    pub async fn ln_send_max(
        &self,
        gateway: &GatewayInfoWrapper,
        lnurl: String,
    ) -> Result<String, String> {
        self.client
            .ln()
            .send_max(
                self.account,
                gateway.gateway_pk,
                gateway.gateway_info.clone(),
                &lnurl,
            )
            .await
            .map(|op| op.to_string())
            .map_err(|e| e.to_string())
    }

    /// Reads the locally mirrored gateway set, so it never touches the network.
    #[frb(sync)]
    pub fn lnurl(&self) -> String {
        self.client
            .ln()
            .generate_lnurl(self.account, "http://159.223.25.182:8082/".to_string())
    }

    #[frb]
    pub async fn onchain_calculate_fees(
        &self,
        _address: &BitcoinAddressWrapper,
        _amount_sats: i64,
    ) -> Result<i64, String> {
        // Picomint's wallet quotes a flat per-tx fee independent of
        // address/amount. Match the existing UI signature; ignore the
        // extra inputs.
        self.client
            .wallet()
            .send_fee()
            .await
            .map(|fee| fee.to_sat() as i64)
            .map_err(|e| e.to_string())
    }

    #[frb]
    pub async fn onchain_send(
        &self,
        address: &BitcoinAddressWrapper,
        amount_sats: i64,
    ) -> Result<(), String> {
        self.client
            .wallet()
            .send(
                self.account,
                address.0.clone(),
                BtcAmount::from_sat(amount_sats as u64),
                None,
            )
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// The largest whole-sat amount this account can send onchain: what its
    /// notes deliver when spent in full, less the onchain fee at the current
    /// consensus feerate, the transaction fee and the app's cut.
    ///
    /// Picomint's quote, from the same code [`Self::onchain_send_max`] sizes
    /// with. The two can differ by a feerate that moved in between — the
    /// send recomputes at the moment it is submitted, and that figure is the
    /// one that goes out.
    #[frb]
    pub async fn onchain_max_amount(&self) -> Result<i64, String> {
        self.client
            .wallet()
            .send_max_amount(self.account)
            .await
            .map(|amount| amount.to_sat() as i64)
            .map_err(|e| e.to_string())
    }

    /// Sends everything to `address`, leaving the account empty. The amount is
    /// picomint's to compute — it funds from every note and sizes the output
    /// to what they cover — so nothing is passed in but where it goes.
    #[frb]
    pub async fn onchain_send_max(&self, address: &BitcoinAddressWrapper) -> Result<(), String> {
        self.client
            .wallet()
            .send_max(self.account, address.0.clone())
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    #[frb]
    pub async fn onchain_receive_address(&self) -> Result<String, String> {
        Ok(self.client.wallet().receive(self.account).await.to_string())
    }
}
