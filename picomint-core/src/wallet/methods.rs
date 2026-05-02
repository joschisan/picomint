//! Wallet module wire methods.
//!
//! Each method has a `Request` and a `Response` type. The [`WalletMethod`] enum
//! ties them together.

use picomint_encoding::{Decodable, Encodable};

use crate::OutPoint;
use crate::wallet::{FederationWallet, OutputInfo, TxInfo};

// ── consensus-block-count ───────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct ConsensusBlockCountRequest;

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct ConsensusBlockCountResponse {
    pub count: u64,
}

// ── consensus-feerate ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct ConsensusFeerateRequest;

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct ConsensusFeerateResponse {
    pub feerate: Option<u64>,
}

// ── federation-wallet ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct FederationWalletRequest;

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct FederationWalletResponse {
    pub wallet: Option<FederationWallet>,
}

// ── send-fee ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct SendFeeRequest;

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct SendFeeResponse {
    pub fee: Option<bitcoin::Amount>,
}

// ── receive-fee ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct ReceiveFeeRequest;

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct ReceiveFeeResponse {
    pub fee: Option<bitcoin::Amount>,
}

// ── transaction-id ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct TxIdRequest {
    pub outpoint: OutPoint,
}

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct TxIdResponse {
    pub txid: Option<bitcoin::Txid>,
}

// ── output-info-slice ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct OutputInfoSliceRequest {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct OutputInfoSliceResponse {
    pub outputs: Vec<OutputInfo>,
}

// ── pending-transaction-chain ───────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct PendingTxChainRequest;

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct PendingTxChainResponse {
    pub txs: Vec<TxInfo>,
}

// ── transaction-chain ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct TxChainRequest;

#[derive(Debug, Clone, Eq, PartialEq, Encodable, Decodable)]
pub struct TxChainResponse {
    pub txs: Vec<TxInfo>,
}

// ── dispatch enum ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Encodable, Decodable)]
pub enum WalletMethod {
    ConsensusBlockCount(ConsensusBlockCountRequest),
    ConsensusFeerate(ConsensusFeerateRequest),
    FederationWallet(FederationWalletRequest),
    SendFee(SendFeeRequest),
    ReceiveFee(ReceiveFeeRequest),
    TxId(TxIdRequest),
    OutputInfoSlice(OutputInfoSliceRequest),
    PendingTxChain(PendingTxChainRequest),
    TxChain(TxChainRequest),
}
