use crate::api::FederationApi;
use picomint_core::OutPoint;
use picomint_core::module::Method;
use picomint_core::wallet::methods::{
    ConsensusFeerateRequest, ConsensusFeerateResponse, FederationWalletRequest,
    FederationWalletResponse, OutputInfoSliceRequest, OutputInfoSliceResponse,
    PendingTxChainRequest, PendingTxChainResponse, ReceiveFeeRequest, ReceiveFeeResponse,
    SendFeeRequest, SendFeeResponse, TxIdRequest, TxIdResponse, WalletMethod,
};
use picomint_core::wallet::{FederationWallet, OutputInfo, TxInfo};

pub async fn consensus_feerate(api: &FederationApi) -> anyhow::Result<Option<u32>> {
    api.request_current_consensus::<ConsensusFeerateResponse>(Method::Wallet(
        WalletMethod::ConsensusFeerate(ConsensusFeerateRequest),
    ))
    .await
    .map(|resp| resp.feerate)
}

pub async fn federation_wallet(api: &FederationApi) -> anyhow::Result<Option<FederationWallet>> {
    api.request_current_consensus::<FederationWalletResponse>(Method::Wallet(
        WalletMethod::FederationWallet(FederationWalletRequest),
    ))
    .await
    .map(|resp| resp.wallet)
}

pub async fn send_fee(api: &FederationApi) -> anyhow::Result<Option<bitcoin::Amount>> {
    api.request_current_consensus::<SendFeeResponse>(Method::Wallet(WalletMethod::SendFee(
        SendFeeRequest,
    )))
    .await
    .map(|resp| resp.fee)
}

pub async fn receive_fee(api: &FederationApi) -> anyhow::Result<Option<bitcoin::Amount>> {
    api.request_current_consensus::<ReceiveFeeResponse>(Method::Wallet(WalletMethod::ReceiveFee(
        ReceiveFeeRequest,
    )))
    .await
    .map(|resp| resp.fee)
}

pub async fn pending_tx_chain(api: &FederationApi) -> anyhow::Result<Vec<TxInfo>> {
    api.request_current_consensus::<PendingTxChainResponse>(Method::Wallet(
        WalletMethod::PendingTxChain(PendingTxChainRequest),
    ))
    .await
    .map(|resp| resp.txs)
}

pub async fn output_info_slice(
    api: &FederationApi,
    start: u64,
    end: u64,
) -> anyhow::Result<Vec<OutputInfo>> {
    api.request_current_consensus::<OutputInfoSliceResponse>(Method::Wallet(
        WalletMethod::OutputInfoSlice(OutputInfoSliceRequest { start, end }),
    ))
    .await
    .map(|resp| resp.outputs)
}

pub async fn tx_id(api: &FederationApi, outpoint: OutPoint) -> Option<bitcoin::Txid> {
    api.request_current_consensus_retry::<TxIdResponse>(Method::Wallet(WalletMethod::TxId(
        TxIdRequest { outpoint },
    )))
    .await
    .txid
}
