//! Freestanding API handlers for [`super::Wallet`].

use picomint_core::module::ApiError;
use picomint_core::wallet::methods::{
    ConsensusBlockCountRequest, ConsensusBlockCountResponse, ConsensusFeerateRequest,
    ConsensusFeerateResponse, FederationWalletRequest, FederationWalletResponse,
    OutputInfoSliceRequest, OutputInfoSliceResponse, PendingTransactionChainRequest,
    PendingTransactionChainResponse, ReceiveFeeRequest, ReceiveFeeResponse, SendFeeRequest,
    SendFeeResponse, TransactionChainRequest, TransactionChainResponse, TransactionIdRequest,
    TransactionIdResponse,
};

use super::Wallet;
use super::db::FEDERATION_WALLET;

pub fn consensus_block_count(
    wallet: &Wallet,
    _: ConsensusBlockCountRequest,
) -> Result<ConsensusBlockCountResponse, ApiError> {
    let tx = wallet.db.begin_read();
    Ok(ConsensusBlockCountResponse {
        count: wallet.consensus_block_count(&tx),
    })
}

pub fn consensus_feerate(
    wallet: &Wallet,
    _: ConsensusFeerateRequest,
) -> Result<ConsensusFeerateResponse, ApiError> {
    let tx = wallet.db.begin_read();
    Ok(ConsensusFeerateResponse {
        feerate: wallet.consensus_feerate(&tx),
    })
}

pub fn federation_wallet(
    wallet: &Wallet,
    _: FederationWalletRequest,
) -> Result<FederationWalletResponse, ApiError> {
    Ok(FederationWalletResponse {
        wallet: wallet.db.begin_read().get(&FEDERATION_WALLET, &()),
    })
}

pub fn send_fee(wallet: &Wallet, _: SendFeeRequest) -> Result<SendFeeResponse, ApiError> {
    Ok(SendFeeResponse {
        fee: wallet.send_fee(&wallet.db.begin_read()),
    })
}

pub fn receive_fee(wallet: &Wallet, _: ReceiveFeeRequest) -> Result<ReceiveFeeResponse, ApiError> {
    Ok(ReceiveFeeResponse {
        fee: wallet.receive_fee(&wallet.db.begin_read()),
    })
}

pub fn tx_id(
    wallet: &Wallet,
    req: TransactionIdRequest,
) -> Result<TransactionIdResponse, ApiError> {
    Ok(TransactionIdResponse {
        txid: Wallet::tx_id(&wallet.db.begin_read(), req.outpoint),
    })
}

pub fn output_info_slice(
    wallet: &Wallet,
    req: OutputInfoSliceRequest,
) -> Result<OutputInfoSliceResponse, ApiError> {
    Ok(OutputInfoSliceResponse {
        outputs: Wallet::get_outputs(&wallet.db.begin_read(), req.start, req.end),
    })
}

pub fn pending_tx_chain(
    wallet: &Wallet,
    _: PendingTransactionChainRequest,
) -> Result<PendingTransactionChainResponse, ApiError> {
    Ok(PendingTransactionChainResponse {
        transactions: Wallet::pending_tx_chain(&wallet.db.begin_read()),
    })
}

pub fn tx_chain(
    wallet: &Wallet,
    _: TransactionChainRequest,
) -> Result<TransactionChainResponse, ApiError> {
    Ok(TransactionChainResponse {
        transactions: Wallet::tx_chain(&wallet.db.begin_read()),
    })
}
