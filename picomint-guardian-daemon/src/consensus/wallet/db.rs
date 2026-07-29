use bitcoin::{TxOut, Txid};
use picomint_core::PeerId;
use picomint_core::wallet::TxInfo;
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::table;
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

/// One peer's entry in a transaction's nonce log — one public nonce pair
/// per tx input.
#[derive(Clone, Debug, Encodable, Decodable)]
pub struct NonceEntry(pub PeerId, pub Vec<tss::PublicNonce>);

picomint_redb::consensus_value!(NonceEntry);

/// One signature share per tx input — wrapped so we can impl `redb::Value`
/// locally.
#[derive(Clone, Debug, Encodable, Decodable)]
pub struct Signatures(pub Vec<tss::SignatureShare>);

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

// The single unsigned transaction the federation is currently signing.
// Further pegins and pegouts are rejected until it completes.
table!(
    UnsignedTxTable,
    () => FederationTx,
    "wallet-unsigned-tx",
);

// Append-only log of the accepted nonce entries for the unsigned
// transaction. Consecutive chunks of threshold entries form the signing
// sessions: session s consists of the entries [s * t, (s + 1) * t). The
// incomplete tail chunk holds the peers available for the next session.
table!(
    NonceLogTable,
    u64 => NonceEntry,
    "wallet-nonce-log",
);

// The signature shares responding to a nonce entry, stored under the same
// index as the entry in the nonce log. Session s is complete once every
// index of its chunk has a response.
table!(
    SignaturesTable,
    u64 => Signatures,
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
