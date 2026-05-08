use bitcoin::{BlockHash, Transaction};
use bitcoincore_rpc::Error::JsonRpc;
use bitcoincore_rpc::bitcoincore_rpc_json::EstimateMode;
use bitcoincore_rpc::jsonrpc::Error::Rpc;
use bitcoincore_rpc::{Auth, Client, RpcApi};
use tokio::task::block_in_place;
use tracing::info;

use crate::Feerate;

#[derive(Debug)]
pub struct BitcoindClient(Client);

impl BitcoindClient {
    pub fn new(username: String, password: String, url: &str) -> anyhow::Result<Self> {
        Ok(Self(Client::new(url, Auth::UserPass(username, password))?))
    }

    pub async fn get_block_count(&self) -> anyhow::Result<u64> {
        // The RPC function is confusingly named and actually returns the block height
        block_in_place(|| self.0.get_block_count())
            .map(|height| height + 1)
            .map_err(anyhow::Error::from)
    }

    pub async fn get_block_hash(&self, height: u64) -> anyhow::Result<BlockHash> {
        block_in_place(|| self.0.get_block_hash(height)).map_err(anyhow::Error::from)
    }

    pub async fn get_block(&self, hash: &BlockHash) -> anyhow::Result<bitcoin::Block> {
        block_in_place(|| self.0.get_block(hash)).map_err(anyhow::Error::from)
    }

    pub async fn get_feerate(&self) -> anyhow::Result<Option<Feerate>> {
        let feerate = block_in_place(|| {
            self.0
                .estimate_smart_fee(1, Some(EstimateMode::Conservative))
        })?
        .fee_rate
        .map(|per_kb| Feerate {
            sats_per_kvb: per_kb.to_sat(),
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
