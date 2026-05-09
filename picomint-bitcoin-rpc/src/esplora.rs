use std::collections::HashMap;

use anyhow::Context;
use bitcoin::{BlockHash, Transaction};
use esplora_client::{AsyncClient, Builder, convert_fee_rate};
use tracing::info;
use url::Url;

use crate::Feerate;

#[derive(Debug)]
pub struct EsploraClient(AsyncClient);

impl EsploraClient {
    pub fn new(url: &Url) -> anyhow::Result<Self> {
        Ok(Self(
            Builder::new(url.as_str().trim_end_matches('/')).build_async()?,
        ))
    }

    pub async fn get_block_count(&self) -> anyhow::Result<u64> {
        match self.0.get_height().await {
            Ok(height) => Ok(u64::from(height) + 1),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn get_block_hash(&self, height: u64) -> anyhow::Result<BlockHash> {
        Ok(self.0.get_block_hash(u32::try_from(height)?).await?)
    }

    pub async fn get_block(&self, block_hash: &BlockHash) -> anyhow::Result<bitcoin::Block> {
        self.0
            .get_block_by_hash(block_hash)
            .await?
            .context("Block with this hash is not available")
    }

    pub async fn get_feerate(&self) -> anyhow::Result<Option<Feerate>> {
        let fee_estimates: HashMap<u16, f64> = self.0.get_fee_estimates().await?;

        let fee_rate_vb = convert_fee_rate(1, fee_estimates).unwrap_or(1.0);

        let fee_rate_kvb = fee_rate_vb * 1_000f32;

        Ok(Some(Feerate {
            sats_per_kvb: (fee_rate_kvb).ceil() as u64,
        }))
    }

    pub async fn submit_tx(&self, tx: Transaction) {
        let _ = self.0.broadcast(&tx).await.map_err(|err| {
            info!(err = %err, "Error broadcasting transaction");
        });
    }

    pub async fn get_sync_progress(&self) -> anyhow::Result<Option<f64>> {
        Ok(None)
    }
}
