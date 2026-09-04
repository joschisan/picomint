//! Freestanding API handlers for the wallet module.

use picomint_core::onchain::methods::{
    ConsensusFeerateRequest, ConsensusFeerateResponse, MintUtxoRequest, MintUtxoResponse,
    OutputInfoSliceRequest, OutputInfoSliceResponse, PendingTxChainRequest, PendingTxChainResponse,
    ReceiveFeeRequest, ReceiveFeeResponse, SendFeeRequest, SendFeeResponse, TxChainRequest,
    TxChainResponse, TxIdRequest, TxIdResponse,
};

use crate::consensus::server::Server;

pub fn consensus_feerate(
    server: &Server,
    _: ConsensusFeerateRequest,
) -> Result<ConsensusFeerateResponse, String> {
    let dbtx = server.db.begin_read();
    Ok(ConsensusFeerateResponse {
        feerate: super::consensus_feerate(server, &dbtx),
    })
}

pub fn mint_utxo(server: &Server, _: MintUtxoRequest) -> Result<MintUtxoResponse, String> {
    Ok(MintUtxoResponse {
        utxo: super::mint_utxo(&server.db.begin_read()),
    })
}

pub fn send_fee(server: &Server, _: SendFeeRequest) -> Result<SendFeeResponse, String> {
    Ok(SendFeeResponse {
        fee: super::send_fee(server, &server.db.begin_read()),
    })
}

pub fn receive_fee(server: &Server, _: ReceiveFeeRequest) -> Result<ReceiveFeeResponse, String> {
    Ok(ReceiveFeeResponse {
        fee: super::receive_fee(server, &server.db.begin_read()),
    })
}

pub fn tx_id(server: &Server, req: TxIdRequest) -> Result<TxIdResponse, String> {
    Ok(TxIdResponse {
        txid: super::tx_id(&server.db.begin_read(), req.outpoint),
    })
}

pub fn output_info_slice(
    server: &Server,
    req: OutputInfoSliceRequest,
) -> Result<OutputInfoSliceResponse, String> {
    Ok(OutputInfoSliceResponse {
        outputs: super::get_outputs(&server.db.begin_read(), req.start, req.end),
    })
}

pub fn pending_tx_chain(
    server: &Server,
    _: PendingTxChainRequest,
) -> Result<PendingTxChainResponse, String> {
    Ok(PendingTxChainResponse {
        txs: super::pending_tx_chain(&server.db.begin_read()),
    })
}

pub fn tx_chain(server: &Server, _: TxChainRequest) -> Result<TxChainResponse, String> {
    Ok(TxChainResponse {
        txs: super::tx_chain(&server.db.begin_read()),
    })
}
