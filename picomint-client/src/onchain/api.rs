use crate::api::MintApi;
use picomint_core::OutPoint;
use picomint_core::module::Method;
use picomint_core::onchain::methods::{
    ConsensusFeerateRequest, ConsensusFeerateResponse, MintUtxoRequest,
    MintUtxoResponse, OutputInfoSliceRequest, OutputInfoSliceResponse,
    PendingTxChainRequest, PendingTxChainResponse, ReceiveFeeRequest, ReceiveFeeResponse,
    SendFeeRequest, SendFeeResponse, TxIdRequest, TxIdResponse, OnchainMethod,
};
use picomint_core::onchain::{MintUtxo, OutputInfo, TxInfo};

pub async fn consensus_feerate(api: &MintApi) -> anyhow::Result<Option<u32>> {
    api.request_current_consensus::<ConsensusFeerateResponse>(Method::Onchain(
        OnchainMethod::ConsensusFeerate(ConsensusFeerateRequest),
    ))
    .await
    .map(|resp| resp.feerate)
}

pub async fn mint_utxo(api: &MintApi) -> anyhow::Result<Option<MintUtxo>> {
    api.request_current_consensus::<MintUtxoResponse>(Method::Onchain(
        OnchainMethod::MintUtxo(MintUtxoRequest),
    ))
    .await
    .map(|resp| resp.utxo)
}

pub async fn send_fee(api: &MintApi) -> anyhow::Result<Option<bitcoin::Amount>> {
    api.request_current_consensus::<SendFeeResponse>(Method::Onchain(OnchainMethod::SendFee(
        SendFeeRequest,
    )))
    .await
    .map(|resp| resp.fee)
}

pub async fn receive_fee(api: &MintApi) -> anyhow::Result<Option<bitcoin::Amount>> {
    api.request_current_consensus::<ReceiveFeeResponse>(Method::Onchain(OnchainMethod::ReceiveFee(
        ReceiveFeeRequest,
    )))
    .await
    .map(|resp| resp.fee)
}

pub async fn pending_tx_chain(api: &MintApi) -> anyhow::Result<Vec<TxInfo>> {
    api.request_current_consensus::<PendingTxChainResponse>(Method::Onchain(
        OnchainMethod::PendingTxChain(PendingTxChainRequest),
    ))
    .await
    .map(|resp| resp.txs)
}

pub async fn output_info_slice(
    api: &MintApi,
    start: u64,
    end: u64,
) -> anyhow::Result<Vec<OutputInfo>> {
    api.request_current_consensus::<OutputInfoSliceResponse>(Method::Onchain(
        OnchainMethod::OutputInfoSlice(OutputInfoSliceRequest { start, end }),
    ))
    .await
    .map(|resp| resp.outputs)
}

pub async fn tx_id(api: &MintApi, outpoint: OutPoint) -> Option<bitcoin::Txid> {
    api.request_current_consensus_retry::<TxIdResponse>(Method::Onchain(OnchainMethod::TxId(
        TxIdRequest { outpoint },
    )))
    .await
    .txid
}
