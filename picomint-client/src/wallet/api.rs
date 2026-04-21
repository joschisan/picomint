use crate::api::{FederationApi, FederationResult};
use picomint_core::OutPoint;
use picomint_core::module::ApiRequestErased;
use picomint_core::wallet::methods::{
    METHOD_CONSENSUS_BLOCK_COUNT, METHOD_CONSENSUS_FEERATE, METHOD_FEDERATION_WALLET,
    METHOD_OUTPUT_INFO_SLICE, METHOD_PENDING_TRANSACTION_CHAIN, METHOD_RECEIVE_FEE,
    METHOD_SEND_FEE, METHOD_TRANSACTION_ID,
};
use picomint_core::wallet::{FederationWallet, OutputInfo, TxInfo};

impl FederationApi {
    pub async fn wallet_consensus_block_count(&self) -> FederationResult<u64> {
        self.request_current_consensus(
            METHOD_CONSENSUS_BLOCK_COUNT.to_string(),
            ApiRequestErased::new(()),
        )
        .await
    }

    pub async fn wallet_consensus_feerate(&self) -> FederationResult<Option<u64>> {
        self.request_current_consensus(
            METHOD_CONSENSUS_FEERATE.to_string(),
            ApiRequestErased::new(()),
        )
        .await
    }

    pub async fn wallet_federation_wallet(&self) -> FederationResult<Option<FederationWallet>> {
        self.request_current_consensus(
            METHOD_FEDERATION_WALLET.to_string(),
            ApiRequestErased::new(()),
        )
        .await
    }

    pub async fn wallet_send_fee(&self) -> FederationResult<Option<bitcoin::Amount>> {
        self.request_current_consensus(METHOD_SEND_FEE.to_string(), ApiRequestErased::new(()))
            .await
    }

    pub async fn wallet_receive_fee(&self) -> FederationResult<Option<bitcoin::Amount>> {
        self.request_current_consensus(METHOD_RECEIVE_FEE.to_string(), ApiRequestErased::new(()))
            .await
    }

    pub async fn wallet_pending_tx_chain(&self) -> FederationResult<Vec<TxInfo>> {
        self.request_current_consensus(
            METHOD_PENDING_TRANSACTION_CHAIN.to_string(),
            ApiRequestErased::new(()),
        )
        .await
    }

    pub async fn wallet_output_info_slice(
        &self,
        start_index: u64,
        end_index: u64,
    ) -> FederationResult<Vec<OutputInfo>> {
        self.request_current_consensus(
            METHOD_OUTPUT_INFO_SLICE.to_string(),
            ApiRequestErased::new((start_index, end_index)),
        )
        .await
    }

    pub async fn wallet_tx_id(&self, outpoint: OutPoint) -> Option<bitcoin::Txid> {
        self.request_current_consensus_retry(
            METHOD_TRANSACTION_ID.to_string(),
            ApiRequestErased::new(outpoint),
        )
        .await
    }
}
