use crate::api::{FederationApi, FederationResult};
use picomint_core::OutPoint;
use picomint_core::module::ApiMethod;
use picomint_core::wallet::methods::{
    ConsensusBlockCountRequest, ConsensusBlockCountResponse, ConsensusFeerateRequest,
    ConsensusFeerateResponse, FederationWalletRequest, FederationWalletResponse,
    OutputInfoSliceRequest, OutputInfoSliceResponse, PendingTransactionChainRequest,
    PendingTransactionChainResponse, ReceiveFeeRequest, ReceiveFeeResponse, SendFeeRequest,
    SendFeeResponse, TransactionIdRequest, TransactionIdResponse, WalletMethod,
};
use picomint_core::wallet::{FederationWallet, OutputInfo, TxInfo};

impl FederationApi {
    pub async fn wallet_consensus_block_count(&self) -> FederationResult<u64> {
        self.request_current_consensus::<ConsensusBlockCountResponse>(ApiMethod::Wallet(
            WalletMethod::ConsensusBlockCount(ConsensusBlockCountRequest),
        ))
        .await
        .map(|resp| resp.count)
    }

    pub async fn wallet_consensus_feerate(&self) -> FederationResult<Option<u64>> {
        self.request_current_consensus::<ConsensusFeerateResponse>(ApiMethod::Wallet(
            WalletMethod::ConsensusFeerate(ConsensusFeerateRequest),
        ))
        .await
        .map(|resp| resp.feerate)
    }

    pub async fn wallet_federation_wallet(&self) -> FederationResult<Option<FederationWallet>> {
        self.request_current_consensus::<FederationWalletResponse>(ApiMethod::Wallet(
            WalletMethod::FederationWallet(FederationWalletRequest),
        ))
        .await
        .map(|resp| resp.wallet)
    }

    pub async fn wallet_send_fee(&self) -> FederationResult<Option<bitcoin::Amount>> {
        self.request_current_consensus::<SendFeeResponse>(ApiMethod::Wallet(WalletMethod::SendFee(
            SendFeeRequest,
        )))
        .await
        .map(|resp| resp.fee)
    }

    pub async fn wallet_receive_fee(&self) -> FederationResult<Option<bitcoin::Amount>> {
        self.request_current_consensus::<ReceiveFeeResponse>(ApiMethod::Wallet(
            WalletMethod::ReceiveFee(ReceiveFeeRequest),
        ))
        .await
        .map(|resp| resp.fee)
    }

    pub async fn wallet_pending_tx_chain(&self) -> FederationResult<Vec<TxInfo>> {
        self.request_current_consensus::<PendingTransactionChainResponse>(ApiMethod::Wallet(
            WalletMethod::PendingTransactionChain(PendingTransactionChainRequest),
        ))
        .await
        .map(|resp| resp.transactions)
    }

    pub async fn wallet_output_info_slice(
        &self,
        start: u64,
        end: u64,
    ) -> FederationResult<Vec<OutputInfo>> {
        self.request_current_consensus::<OutputInfoSliceResponse>(ApiMethod::Wallet(
            WalletMethod::OutputInfoSlice(OutputInfoSliceRequest { start, end }),
        ))
        .await
        .map(|resp| resp.outputs)
    }

    pub async fn wallet_tx_id(&self, outpoint: OutPoint) -> Option<bitcoin::Txid> {
        self.request_current_consensus_retry::<TransactionIdResponse>(ApiMethod::Wallet(
            WalletMethod::TransactionId(TransactionIdRequest { outpoint }),
        ))
        .await
        .txid
    }
}
