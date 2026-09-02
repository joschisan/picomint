use bitcoin::{TxOut, Txid};
use picomint_core::PeerId;
use picomint_core::wallet::TxInfo;
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::table;
use serde::Serialize;

use super::{FederationTx, FederationWallet};

#[derive(Clone, Debug, Eq, PartialEq, Encodable, Decodable, Serialize)]
pub struct Output(pub bitcoin::OutPoint, pub TxOut);

/// One peer's vote for an entry of the tracked output log.
#[derive(Clone, Debug, Encodable, Decodable)]
pub struct OutputVote(pub PeerId, pub Output);

/// One peer's entry in a transaction's nonce log — one public nonce pair
/// per tx input.
#[derive(Clone, Debug, Encodable, Decodable)]
pub struct NonceEntry(pub PeerId, pub Vec<tss::PublicNonce>);

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

// The signature shares responding to a nonce entry — one share per tx
// input — stored under the same index as the entry in the nonce log.
// Session s is complete once every index of its chunk has a response.
table!(
    SignaturesTable,
    u64 => Vec<tss::SignatureShare>,
    "wallet-signatures",
);

table!(
    UnconfirmedTxTable,
    Txid => FederationTx,
    "wallet-unconfirmed-tx",
);

table!(
    BlockCountVoteTable,
    PeerId => u64,
    "wallet-block-count-vote",
);

// The first block height the federation tracks — the first nonzero
// consensus block count. Blocks before it predate the federation and are
// never scanned, and every peer's observed output log starts here, which
// is what lines the log indexes up across peers.
table!(
    StartHeightTable,
    () => u64,
    "wallet-start-height",
);

// One peer's next unvoted index into its observed output log, advanced by
// every accepted vote. Sequencing per peer is what makes proposing a
// single counter comparison and caps every peer at one vote per index.
table!(
    OutputVotePositionTable,
    PeerId => u64,
    "wallet-output-vote-position",
);

// The pending votes for output log indexes at or past the tracked head,
// evicted once their index is tracked.
table!(
    OutputVoteTable,
    u64 => Vec<OutputVote>,
    "wallet-output-vote",
);

// The peers that have seen an unconfirmed federation transaction buried
// under the finality delay; at threshold the transaction is retired from
// UNCONFIRMED_TX and the votes are evicted.
table!(
    ConfirmedVoteTable,
    Txid => Vec<PeerId>,
    "wallet-confirmed-vote",
);

// Local, not consensus state: this guardian's observed output log,
// written by the block scanner in block order.
table!(
    ObservedOutputTable,
    u64 => Output,
    "wallet-observed-output",
);

// Local: the pending federation txids this guardian has seen buried under
// the finality delay.
table!(
    ObservedConfirmedTable,
    Txid => (),
    "wallet-observed-confirmed",
);

// Local: the next block height the scanner will read.
table!(
    ScanCursorTable,
    () => u64,
    "wallet-scan-cursor",
);

table!(
    FeeRateVoteTable,
    PeerId => Option<u64>,
    "wallet-fee-rate-vote",
);
