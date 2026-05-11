//! Freestanding API handlers for [`super::Wallet`].

use picomint_core::wallet::methods::{
    ConsensusBlockCountRequest, ConsensusBlockCountResponse, ConsensusFeerateRequest,
    ConsensusFeerateResponse, FederationWalletRequest, FederationWalletResponse,
    OutputInfoSliceRequest, OutputInfoSliceResponse, PendingTxChainRequest, PendingTxChainResponse,
    ReceiveFeeRequest, ReceiveFeeResponse, SendFeeRequest, SendFeeResponse, TxChainRequest,
    TxChainResponse, TxIdRequest, TxIdResponse,
};

use super::Wallet;
use super::db::FEDERATION_WALLET;

pub fn consensus_block_count(
    wallet: &Wallet,
    _: ConsensusBlockCountRequest,
) -> Result<ConsensusBlockCountResponse, String> {
    let dbtx = wallet.db.begin_read();
    Ok(ConsensusBlockCountResponse {
        count: wallet.consensus_block_count(&dbtx),
    })
}

pub fn consensus_feerate(
    wallet: &Wallet,
    _: ConsensusFeerateRequest,
) -> Result<ConsensusFeerateResponse, String> {
    let dbtx = wallet.db.begin_read();
    Ok(ConsensusFeerateResponse {
        feerate: wallet.consensus_feerate(&dbtx),
    })
}

pub fn federation_wallet(
    wallet: &Wallet,
    _: FederationWalletRequest,
) -> Result<FederationWalletResponse, String> {
    Ok(FederationWalletResponse {
        wallet: wallet.db.begin_read().get(&FEDERATION_WALLET, &()),
    })
}

pub fn send_fee(wallet: &Wallet, _: SendFeeRequest) -> Result<SendFeeResponse, String> {
    Ok(SendFeeResponse {
        fee: wallet.send_fee(&wallet.db.begin_read()),
    })
}

pub fn receive_fee(wallet: &Wallet, _: ReceiveFeeRequest) -> Result<ReceiveFeeResponse, String> {
    Ok(ReceiveFeeResponse {
        fee: wallet.receive_fee(&wallet.db.begin_read()),
    })
}

pub fn tx_id(wallet: &Wallet, req: TxIdRequest) -> Result<TxIdResponse, String> {
    Ok(TxIdResponse {
        txid: Wallet::tx_id(&wallet.db.begin_read(), req.outpoint),
    })
}

pub fn output_info_slice(
    wallet: &Wallet,
    req: OutputInfoSliceRequest,
) -> Result<OutputInfoSliceResponse, String> {
    Ok(OutputInfoSliceResponse {
        outputs: Wallet::get_outputs(&wallet.db.begin_read(), req.start, req.end),
    })
}

pub fn pending_tx_chain(
    wallet: &Wallet,
    _: PendingTxChainRequest,
) -> Result<PendingTxChainResponse, String> {
    Ok(PendingTxChainResponse {
        txs: Wallet::pending_tx_chain(&wallet.db.begin_read()),
    })
}

pub fn tx_chain(wallet: &Wallet, _: TxChainRequest) -> Result<TxChainResponse, String> {
    Ok(TxChainResponse {
        txs: Wallet::tx_chain(&wallet.db.begin_read()),
    })
}
