use crate::api::FederationApi;
use picomint_core::OutPoint;
use picomint_core::module::Method;
use picomint_core::onchain::methods::{
    ConsensusFeerateRequest, ConsensusFeerateResponse, FederationUtxoRequest,
    FederationUtxoResponse, OutputInfoSliceRequest, OutputInfoSliceResponse,
    PendingTxChainRequest, PendingTxChainResponse, ReceiveFeeRequest, ReceiveFeeResponse,
    SendFeeRequest, SendFeeResponse, TxIdRequest, TxIdResponse, OnchainMethod,
};
use picomint_core::onchain::{FederationUtxo, OutputInfo, TxInfo};

pub async fn consensus_feerate(api: &FederationApi) -> anyhow::Result<Option<u32>> {
    api.request_current_consensus::<ConsensusFeerateResponse>(Method::Onchain(
        OnchainMethod::ConsensusFeerate(ConsensusFeerateRequest),
    ))
    .await
    .map(|resp| resp.feerate)
}

pub async fn federation_utxo(api: &FederationApi) -> anyhow::Result<Option<FederationUtxo>> {
    api.request_current_consensus::<FederationUtxoResponse>(Method::Onchain(
        OnchainMethod::FederationUtxo(FederationUtxoRequest),
    ))
    .await
    .map(|resp| resp.utxo)
}

pub async fn send_fee(api: &FederationApi) -> anyhow::Result<Option<bitcoin::Amount>> {
    api.request_current_consensus::<SendFeeResponse>(Method::Onchain(OnchainMethod::SendFee(
        SendFeeRequest,
    )))
    .await
    .map(|resp| resp.fee)
}

pub async fn receive_fee(api: &FederationApi) -> anyhow::Result<Option<bitcoin::Amount>> {
    api.request_current_consensus::<ReceiveFeeResponse>(Method::Onchain(OnchainMethod::ReceiveFee(
        ReceiveFeeRequest,
    )))
    .await
    .map(|resp| resp.fee)
}

pub async fn pending_tx_chain(api: &FederationApi) -> anyhow::Result<Vec<TxInfo>> {
    api.request_current_consensus::<PendingTxChainResponse>(Method::Onchain(
        OnchainMethod::PendingTxChain(PendingTxChainRequest),
    ))
    .await
    .map(|resp| resp.txs)
}

pub async fn output_info_slice(
    api: &FederationApi,
    start: u64,
    end: u64,
) -> anyhow::Result<Vec<OutputInfo>> {
    api.request_current_consensus::<OutputInfoSliceResponse>(Method::Onchain(
        OnchainMethod::OutputInfoSlice(OutputInfoSliceRequest { start, end }),
    ))
    .await
    .map(|resp| resp.outputs)
}

pub async fn tx_id(api: &FederationApi, outpoint: OutPoint) -> Option<bitcoin::Txid> {
    api.request_current_consensus_retry::<TxIdResponse>(Method::Onchain(OnchainMethod::TxId(
        TxIdRequest { outpoint },
    )))
    .await
    .txid
}
