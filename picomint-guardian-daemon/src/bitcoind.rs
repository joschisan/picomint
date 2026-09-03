//! Bitcoind RPC client + background status monitor.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use bitcoincore_rpc::Error::JsonRpc;
use bitcoincore_rpc::bitcoincore_rpc_json::EstimateMode;
use bitcoincore_rpc::jsonrpc::Error::Rpc;
use bitcoincore_rpc::{Auth, Client, RpcApi};
use picomint_core::bitcoin::{Block, BlockHash, Network, Transaction};
use tokio::sync::watch;
use tokio::task::block_in_place;
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

#[derive(Debug)]
pub struct BitcoindClient(Client);

impl BitcoindClient {
    pub fn new(url: &Url) -> anyhow::Result<Self> {
        let username = url.username().to_owned();
        let password = url
            .password()
            .context("BITCOIND_URL must embed credentials: http://user:pass@host")?
            .to_owned();

        Ok(Self(Client::new(
            url.as_str(),
            Auth::UserPass(username, password),
        )?))
    }

    pub async fn get_block_count(&self) -> anyhow::Result<u32> {
        // The RPC function is confusingly named and actually returns the block height
        block_in_place(|| self.0.get_block_count())
            .map(|height| u32::try_from(height + 1).expect("Bitcoin block heights fit in a u32"))
            .map_err(anyhow::Error::from)
    }

    pub async fn get_block_hash(&self, height: u32) -> anyhow::Result<BlockHash> {
        block_in_place(|| self.0.get_block_hash(height.into())).map_err(anyhow::Error::from)
    }

    pub async fn get_block(&self, hash: &BlockHash) -> anyhow::Result<Block> {
        block_in_place(|| self.0.get_block(hash)).map_err(anyhow::Error::from)
    }

    pub async fn get_feerate(&self) -> anyhow::Result<Option<Feerate>> {
        let feerate = block_in_place(|| {
            self.0
                .estimate_smart_fee(1, Some(EstimateMode::Conservative))
        })?
        .fee_rate
        .map(|per_kb| Feerate {
            sat_per_kvb: u32::try_from(per_kb.to_sat())
                .expect("bitcoind feerate estimates fit u32 sat/kvb"),
        });

        Ok(feerate)
    }

    pub async fn submit_tx(&self, tx: Transaction) {
        match block_in_place(|| self.0.send_raw_transaction(&tx)) {
            // Bitcoin core's RPC will return error code -27 if a transaction is already in a block.
            // This is considered a success case, so we don't surface the error log.
            //
            // https://github.com/bitcoin/bitcoin/blob/daa56f7f665183bcce3df146f143be37f33c123e/src/rpc/protocol.h#L48
            Err(JsonRpc(Rpc(e))) if e.code == -27 => (),
            Err(e) => {
                info!(e = %e, "Error broadcasting transaction")
            }
            Ok(_) => (),
        }
    }

    pub async fn get_sync_progress(&self) -> anyhow::Result<Option<f64>> {
        Ok(Some(
            block_in_place(|| self.0.get_blockchain_info())?.verification_progress,
        ))
    }
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

        let rpc_clone = rpc.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(update_interval);
            loop {
                interval.tick().await;
                match Self::fetch_status(&rpc_clone).await {
                    Ok(new_status) => {
                        status_tx.send_replace(Some(new_status));
                    }
                    Err(err) => {
                        warn!(
                            err = %format_args!("{err:#}"),
                            "Bitcoin status update failed"
                        );
                        status_tx.send_replace(None);
                    }
                }
            }
        });

        Self { rpc, status_rx }
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
        ensure!(
            self.status_rx.borrow().is_some(),
            "Not connected to bitcoin backend"
        );

        self.rpc.get_block(hash).await
    }

    pub async fn get_block_hash(&self, height: u32) -> Result<BlockHash> {
        ensure!(
            self.status_rx.borrow().is_some(),
            "Not connected to bitcoin backend"
        );

        self.rpc.get_block_hash(height).await
    }

    pub async fn submit_tx(&self, tx: Transaction) {
        if self.status_rx.borrow().is_some() {
            self.rpc.submit_tx(tx).await;
        }
    }
}
