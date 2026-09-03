//! Bitcoind RPC client + background status monitor.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use picomint_core::bitcoin::consensus::encode::{deserialize_hex, serialize_hex};
use picomint_core::bitcoin::{Block, BlockHash, Network, Transaction};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::watch;
use tracing::{info, warn};
use url::Url;

// Well-known block-hash-at-height-1 values for the Bitcoin networks we
// recognize. Anything else is assumed to be a regtest / custom chain.
// <https://blockstream.info/api/block-height/1>
const MAINNET: &str = "00000000839a8e6886ab5951d76f411475428afc90947ee320161bbf18eb6048";
// <https://blockstream.info/testnet/api/block-height/1>
const TESTNET: &str = "00000000b873e79784647a6c82962c70d228557d24a747ea4d1b8bbe878e1206";
// <https://mempool.space/signet/api/block-height/1>
const SIGNET_4: &str = "00000086d6b2636cb2a392d45edc4ec544a10024d30141c9adf4bfd9de533b53";
// <https://mutinynet.com/api/block-height/1>
const MUTINYNET: &str = "000002855893a0a9b24eaffc5efc770558a326fee4fc10c9da22fc19cd2954f9";

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Feerate {
    pub sat_per_kvb: u32,
}

/// Status of the bitcoind backend as reported by the monitor.
#[derive(Debug, Clone)]
pub struct BitcoindRpcStatus {
    pub network: Network,
    pub block_count: u32,
    /// `None` while the backend is still syncing — fee estimation has no
    /// data until the node is at the tip, and consensus (the only consumer
    /// that needs a feerate) doesn't start until then either.
    pub fee_rate: Option<Feerate>,
    pub sync_progress: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct BitcoindRpcMonitor {
    rpc: Arc<BitcoindClient>,
    status_rx: watch::Receiver<Option<BitcoindRpcStatus>>,
}

impl BitcoindRpcMonitor {
    pub fn new(rpc: Arc<BitcoindClient>, update_interval: Duration) -> Self {
        let (status_tx, status_rx) = watch::channel(None);

        tokio::spawn(Self::update_status(rpc.clone(), update_interval, status_tx));

        Self { rpc, status_rx }
    }

    async fn update_status(
        rpc: Arc<BitcoindClient>,
        update_interval: Duration,
        status_tx: watch::Sender<Option<BitcoindRpcStatus>>,
    ) {
        let mut interval = tokio::time::interval(update_interval);

        loop {
            let status = Self::fetch_status(&rpc)
                .await
                .inspect_err(|e| warn!(?e, "Bitcoin status update failed"))
                .ok();

            status_tx.send_replace(status);

            interval.tick().await;
        }
    }

    async fn fetch_status(rpc: &BitcoindClient) -> Result<BitcoindRpcStatus> {
        let network = match rpc.get_block_hash(1).await?.to_string().as_str() {
            MAINNET => Network::Bitcoin,
            TESTNET => Network::Testnet,
            SIGNET_4 | MUTINYNET => Network::Signet,
            _ => Network::Regtest,
        };

        let block_count = rpc.get_block_count().await?;

        let sync_progress = rpc.get_sync_progress().await?;

        let fee_rate = if network == Network::Regtest {
            Some(Feerate { sat_per_kvb: 1000 })
        } else {
            rpc.get_feerate().await?
        };

        Ok(BitcoindRpcStatus {
            network,
            block_count,
            fee_rate,
            sync_progress,
        })
    }

    pub fn status(&self) -> Option<BitcoindRpcStatus> {
        self.status_rx.borrow().clone()
    }

    pub async fn get_block(&self, hash: &BlockHash) -> Result<Block> {
        self.rpc.get_block(hash).await
    }

    pub async fn get_block_hash(&self, height: u32) -> Result<BlockHash> {
        self.rpc.get_block_hash(height).await
    }

    pub async fn submit_tx(&self, tx: Transaction) {
        if self.status_rx.borrow().is_some() {
            self.rpc.submit_tx(tx).await;
        }
    }
}

#[derive(Deserialize)]
struct RpcResponse<T> {
    result: Option<T>,
    error: Option<RpcError>,
}

/// JSON-RPC error object returned by bitcoind. A typed error so callers
/// can match on well-known codes via [`anyhow::Error::downcast_ref`].
#[derive(Debug, Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

/// Subset of the `estimatesmartfee` response; `feerate` is in BTC/kvB
/// and absent while the node has no estimate yet.
#[derive(Deserialize)]
struct EstimateSmartFee {
    feerate: Option<f64>,
}

/// Subset of the `getblockchaininfo` response.
#[derive(Deserialize)]
struct BlockchainInfo {
    verificationprogress: f64,
}

#[derive(Debug)]
pub struct BitcoindClient {
    client: reqwest::Client,
    /// Keeps its embedded credentials — reqwest extracts userinfo from the
    /// url into a basic auth header on every request.
    url: Url,
}

impl BitcoindClient {
    pub fn new(url: Url) -> Self {
        Self {
            client: reqwest::Client::new(),
            url,
        }
    }

    async fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> anyhow::Result<T> {
        let request = json!({
            "jsonrpc": "1.0",
            "id": "picomint",
            "method": method,
            "params": params,
        });

        let http_response = self
            .client
            .post(self.url.clone())
            .json(&request)
            .send()
            .await?;

        let status = http_response.status();

        // Bitcoind signals RPC errors with a non-success status but still
        // sends the JSON-RPC error envelope, so decode before checking the
        // status and only surface it when there is no envelope to blame.
        let response: RpcResponse<T> = http_response
            .json()
            .await
            .with_context(|| format!("bitcoind returned {status} with a non-JSON-RPC body"))?;

        match (response.result, response.error) {
            (Some(result), None) => Ok(result),
            (_, Some(error)) => Err(anyhow::Error::new(error)),
            _ => bail!("JSON-RPC response carries neither result nor error"),
        }
    }

    pub async fn get_block_count(&self) -> anyhow::Result<u32> {
        // The RPC method is confusingly named and actually returns the block height
        self.call::<u32>("getblockcount", json!([]))
            .await
            .map(|height| height + 1)
    }

    pub async fn get_block_hash(&self, height: u32) -> anyhow::Result<BlockHash> {
        self.call("getblockhash", json!([height])).await
    }

    pub async fn get_block(&self, hash: &BlockHash) -> anyhow::Result<Block> {
        let hex: String = self.call("getblock", json!([hash, 0])).await?;

        Ok(deserialize_hex(&hex)?)
    }

    pub async fn get_feerate(&self) -> anyhow::Result<Option<Feerate>> {
        let response: EstimateSmartFee = self
            .call("estimatesmartfee", json!([1, "CONSERVATIVE"]))
            .await?;

        Ok(response.feerate.map(|btc_per_kvb| Feerate {
            sat_per_kvb: u32::try_from((btc_per_kvb * 100_000_000.0).round() as u64)
                .expect("bitcoind feerate estimates fit u32 sat/kvb"),
        }))
    }

    pub async fn submit_tx(&self, tx: Transaction) {
        match self
            .call::<String>("sendrawtransaction", json!([serialize_hex(&tx)]))
            .await
        {
            // Bitcoin core's RPC will return error code -27 if a transaction is already in a block.
            // This is considered a success case, so we don't surface the error log.
            //
            // https://github.com/bitcoin/bitcoin/blob/daa56f7f665183bcce3df146f143be37f33c123e/src/rpc/protocol.h#L48
            Err(e) if e.downcast_ref::<RpcError>().is_some_and(|e| e.code == -27) => (),
            Err(e) => info!(e = %e, "Error broadcasting transaction"),
            Ok(_) => (),
        }
    }

    pub async fn get_sync_progress(&self) -> anyhow::Result<Option<f64>> {
        self.call::<BlockchainInfo>("getblockchaininfo", json!([]))
            .await
            .map(|info| Some(info.verificationprogress))
    }
}
