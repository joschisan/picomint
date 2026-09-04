use bitcoin::{TxOut, Txid};
use picomint_core::PeerId;
use picomint_core::onchain::TxInfo;
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::table;
use serde::Serialize;

use super::{MintTx, MintUtxo};

#[derive(Clone, Debug, Encodable, Decodable, Serialize)]
pub struct Output(pub bitcoin::OutPoint, pub TxOut);

/// One peer's entry in a transaction's nonce log — one public nonce pair
/// per tx input.
#[derive(Clone, Debug, Encodable, Decodable)]
pub struct NonceEntry(pub PeerId, pub Vec<tss::PublicNonce>);

table!(
    OutputTable,
    u64 => Output,
    "onchain-output",
);

table!(
    SpentOutputTable,
    u64 => (),
    "onchain-spent-output",
);

table!(
    MintWalletTable,
    () => MintUtxo,
    "onchain-mint-utxo",
);

table!(
    TxInfoTable,
    u64 => TxInfo,
    "onchain-tx-info",
);

table!(
    TxInfoIndexTable,
    picomint_core::OutPoint => u64,
    "onchain-tx-info-index",
);

// The single unsigned transaction the mint is currently signing.
// Further pegins and pegouts are rejected until it completes.
table!(
    UnsignedTxTable,
    () => MintTx,
    "onchain-unsigned-tx",
);

// Append-only log of the accepted nonce entries for the unsigned
// transaction. Consecutive chunks of threshold entries form the signing
// sessions: session s consists of the entries [s * t, (s + 1) * t). The
// incomplete tail chunk holds the peers available for the next session.
table!(
    NonceLogTable,
    u64 => NonceEntry,
    "onchain-nonce-log",
);

// The signature shares responding to a nonce entry — one share per tx
// input — stored under the same index as the entry in the nonce log.
// Session s is complete once every index of its chunk has a response.
table!(
    SignaturesTable,
    u64 => Vec<tss::SignatureShare>,
    "onchain-signatures",
);

table!(
    UnconfirmedTxTable,
    Txid => MintTx,
    "onchain-unconfirmed-tx",
);

table!(
    FeeRateVoteTable,
    PeerId => Option<u32>,
    "onchain-fee-rate-vote",
);
