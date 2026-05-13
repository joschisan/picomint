use bitcoin::{TxOut, Txid};
use picomint_core::PeerId;
use picomint_core::wallet::TxInfo;
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::table;
use secp256k1::ecdsa::Signature;
use serde::Serialize;

use super::{FederationTx, FederationWallet};

#[derive(Clone, Debug, Encodable, Decodable, Serialize)]
pub struct Output(pub bitcoin::OutPoint, pub TxOut);

picomint_redb::consensus_value!(Output);

/// Newtype wrapper for `bitcoin::Txid` — lets us impl `redb::Key` locally
/// (orphan rules forbid impling it on the foreign `Txid`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Encodable, Decodable)]
pub struct TxidKey(pub Txid);

picomint_redb::consensus_key!(TxidKey);

/// Vec of ecdsa signatures — wrapped so we can impl `redb::Value` locally.
#[derive(Clone, Debug, Encodable, Decodable)]
pub struct Signatures(pub Vec<Signature>);

picomint_redb::consensus_value!(Signatures);

table!(
    OutputTable,
    u64 => Output,
    "wallet-output",
);

table!(
    SpentOutputTable,
    u64 => (),
    "wallet-spent-output",
);

table!(
    FederationWalletTable,
    () => FederationWallet,
    "wallet-federation-wallet",
);

table!(
    TxInfoTable,
    u64 => TxInfo,
    "wallet-tx-info",
);

table!(
    TxInfoIndexTable,
    picomint_core::OutPoint => u64,
    "wallet-tx-info-index",
);

table!(
    UnsignedTxTable,
    TxidKey => FederationTx,
    "wallet-unsigned-tx",
);

table!(
    SignaturesTable,
    (TxidKey, PeerId) => Signatures,
    "wallet-signatures",
);

table!(
    UnconfirmedTxTable,
    TxidKey => FederationTx,
    "wallet-unconfirmed-tx",
);

table!(
    BlockCountVoteTable,
    PeerId => u64,
    "wallet-block-count-vote",
);

table!(
    FeeRateVoteTable,
    PeerId => Option<u64>,
    "wallet-fee-rate-vote",
);
