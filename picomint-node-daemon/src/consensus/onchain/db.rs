use bitcoin::Txid;
use picomint_core::NodeId;
use picomint_core::onchain::{BlockVote, TrackedOutput, TxInfo};
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::table;

use super::{MintTx, MintUtxo};

/// One node's entry in a transaction's nonce log — one public nonce pair
/// per tx input.
#[derive(Clone, Debug, Encodable, Decodable)]
pub struct NonceEntry(pub NodeId, pub Vec<tss::PublicNonce>);

// The outputs consensus tracks, in chain order under a dense index.
table!(
    OutputTable,
    u64 => TrackedOutput,
    "onchain-output",
);

// The height of the block the mint tracks next: every block below it has
// been tracked.
// Set to the first non-zero consensus block height, so nothing before the
// mint's first block is ever tracked, and absent until then.
table!(
    BlockHeightTable,
    () => u32,
    "onchain-block-height",
);

// Each node's vote on the block at the tracked height, cleared as
// the height advances.
table!(
    BlockVoteTable,
    NodeId => BlockVote,
    "onchain-block-vote",
);

table!(
    SpentOutputIndexTable,
    u64 => (),
    "onchain-spent-output-index",
);

table!(
    MintOnchainTable,
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
// incomplete tail chunk holds the nodes available for the next session.
table!(
    NonceLogTable,
    u64 => NonceEntry,
    "onchain-nonce-log",
);

// The signature shares responding to a nonce entry — one share per tx
// input — stored under the same index as the entry in the nonce log.
// Session s is complete once every index of its chunk has a response.
table!(
    SignatureSharesTable,
    u64 => Vec<tss::SignatureShare>,
    "onchain-signature-shares",
);

table!(
    UnconfirmedTxTable,
    Txid => MintTx,
    "onchain-unconfirmed-tx",
);

table!(
    FeeRateVoteTable,
    NodeId => Option<u32>,
    "onchain-fee-rate-vote",
);
