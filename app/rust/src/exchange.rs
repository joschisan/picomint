use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::sync::Mutex;

#[derive(Deserialize)]
struct FediPriceResponse {
    prices: BTreeMap<String, ExchangeRate>,
}

#[derive(Deserialize)]
struct ExchangeRate {
    rate: f64,
}

/// Every rate from the price feed, keyed by pair (e.g. `"BTC/USD"`,
/// `"EUR/USD"`), cached together with the fetch time. Storing the whole map
/// (rather than a single BTC/selected-currency rate) lets the user switch
/// currency and reprice instantly off the same snapshot — no refetch, no
/// invalidation.
pub(crate) type ExchangeRateCache = Arc<Mutex<Option<(BTreeMap<String, f64>, Instant)>>>;

/// How long a cached snapshot is considered fresh.
pub(crate) const FRESHNESS: Duration = Duration::from_secs(600);

/// BTC price in `currency_code` derived from a cached rate map: `BTC/USD`
/// directly for USD, otherwise `BTC/USD` divided by `{currency}/USD`. `None`
/// when a required pair is absent from the feed.
pub(crate) fn btc_price(prices: &BTreeMap<String, f64>, currency_code: &str) -> Option<f64> {
    let btc_to_usd = prices.get("BTC/USD")?;

    if currency_code == "USD" {
        Some(*btc_to_usd)
    } else {
        let currency_to_usd = prices.get(&format!("{currency_code}/USD"))?;
        Some(btc_to_usd / currency_to_usd)
    }
}

/// Refresh the cached rate map if it's missing or stale, leaving a fresh
/// snapshot untouched. One fetch warms every currency at once.
pub(crate) async fn fetch_exchange_rates(cache: ExchangeRateCache) -> Result<(), String> {
    let mut guard = cache.lock().await;

    if let Some((_, timestamp)) = guard.as_ref()
        && timestamp.elapsed() < FRESHNESS
    {
        return Ok(());
    }

    let response = reqwest::get("https://price-feed.dev.fedibtc.com/latest")
        .await
        .map_err(|_| "Failed to fetch exchange rates".to_string())?
        .json::<FediPriceResponse>()
        .await
        .map_err(|_| "Failed to parse exchange rates".to_string())?;

    let prices = response
        .prices
        .into_iter()
        .map(|(pair, rate)| (pair, rate.rate))
        .collect();

    *guard = Some((prices, Instant::now()));

    Ok(())
}
