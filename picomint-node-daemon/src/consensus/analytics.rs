//! The node's analytics registry: the accepted consensus items exploded
//! into one table per item kind and per input and output kind, mirrored
//! by `picomint-analytics` through [`schema`], [`rows`] and [`reader`].
//!
//! The `accepted-item` table is the log: every item consensus accepted,
//! keyed by session and dense position, written once and never moved.
//! Every row starts with that position and the submitting node, and
//! nothing else — no wallclock, since a restored node replays every
//! session from zero and would stamp its whole history with the replay
//! time. The session is the time axis, and the block height votes are the
//! coarse consensus clock. The mirror is byte-identical on every node.
//!
//! Derived consensus values are not mirrored: the consensus block height
//! at any position is the threshold-th highest of each node's latest
//! vote before it, which is one SQL query over `block_height_vote`.

use picomint_core::lightning::LightningOutput;
use picomint_core::sql::{Row, SqlRow, SqlValue, create_table};
use picomint_core::tx::ConsensusItem;
use picomint_core::version::ConsensusVersion;
use picomint_core::wire::{Input, ModuleConsensusItem, Output};
use picomint_core::{NodeId, TransactionId};
use picomint_redb::{Database, DbRead};

use crate::consensus::db::AcceptedItemTable;
use crate::consensus::server::Server;
use crate::consensus::{ecash, lightning, onchain};

/// A row type with its table: the registry lists one of these per table.
pub trait Table: SqlRow {
    const NAME: &'static str;

    /// Column lists to index, beyond the position every table starts with.
    const INDEXES: &'static [&'static str];
}

/// Every table the mirror holds.
macro_rules! tables {
    ($($row:ty),* $(,)?) => {
        /// The DDL for every registered table.
        pub fn schema() -> String {
            let mut sql = String::new();
            $(sql.push_str(&create_table::<$row>(<$row>::NAME, &COMMON_COLUMNS, <$row>::INDEXES));)*
            sql
        }
    };
}

tables! {
    TxRow,
    BlockHeightVoteRow,
    VersionVoteRow,
    ecash::analytics::InputRow,
    ecash::analytics::OutputRow,
    onchain::analytics::InputRow,
    onchain::analytics::OutputRow,
    onchain::analytics::BlockVoteRow,
    onchain::analytics::FeerateVoteRow,
    onchain::analytics::NoncesRow,
    onchain::analytics::SignatureSharesRow,
    lightning::analytics::InputRow,
    lightning::analytics::OutgoingOutputRow,
    lightning::analytics::IncomingOutputRow,
}

/// The item's position in the log and the node that submitted it, ahead
/// of every table's own columns. Child rows of a transaction repeat them
/// so a query over one table never needs the join.
const COMMON_COLUMNS: [(&str, &str); 3] = [
    ("session", "INTEGER NOT NULL"),
    ("idx", "INTEGER NOT NULL"),
    ("node", "INTEGER NOT NULL"),
];

/// A client transaction; its inputs and outputs are the rows of the
/// module tables that share its `txid`.
#[derive(SqlRow)]
pub struct TxRow {
    pub txid: TransactionId,
    pub inputs: u64,
    pub outputs: u64,
    /// The mint's fee for the transaction, the input and output fees the
    /// consensus config charged for it.
    pub fee: picomint_core::Amount,
}

impl Table for TxRow {
    const NAME: &'static str = "tx";
    const INDEXES: &'static [&'static str] = &["txid"];
}

/// A node's vote for the height of its bitcoin chain tip.
#[derive(SqlRow)]
pub struct BlockHeightVoteRow {
    pub height: u32,
}

impl Table for BlockHeightVoteRow {
    const NAME: &'static str = "block_height_vote";
    const INDEXES: &'static [&'static str] = &["node"];
}

/// A node's vote for the highest consensus version its binary runs.
#[derive(SqlRow)]
pub struct VersionVoteRow {
    pub version: ConsensusVersion,
}

impl Table for VersionVoteRow {
    const NAME: &'static str = "version_vote";
    const INDEXES: &'static [&'static str] = &["node"];
}

/// An accepted item at its position in the log.
pub struct Entry {
    pub session: u32,
    pub idx: u64,
    pub node: NodeId,
    pub item: ConsensusItem,
}

/// One row of `R` at the entry's position.
pub fn row<R: Table>(entry: &Entry, row: &R) -> Row {
    let mut values = vec![
        SqlValue::Integer(i64::from(entry.session)),
        SqlValue::Integer(entry.idx.cast_signed()),
        SqlValue::Integer(entry.node.to_usize() as i64),
    ];

    values.extend(row.values());

    Row {
        table: R::NAME.to_string(),
        values,
    }
}

/// The rows an accepted item lands as.
pub fn rows(server: &Server, entry: &Entry) -> Vec<Row> {
    match &entry.item {
        ConsensusItem::Tx(tx) => {
            let txid = tx.compute_txid();

            let fee = tx
                .inputs
                .iter()
                .map(|input| server.input_fee(input))
                .chain(tx.outputs.iter().map(|output| server.output_fee(output)))
                .sum();

            let tx_row = TxRow {
                txid,
                inputs: tx.inputs.len() as u64,
                outputs: tx.outputs.len() as u64,
                fee,
            };

            std::iter::once(row(entry, &tx_row))
                .chain(tx.inputs.iter().zip(0u16..).map(|input| match input.0 {
                    Input::Ecash(i) => row(entry, &ecash::analytics::input_row(txid, input.1, i)),
                    Input::Onchain(i) => {
                        row(entry, &onchain::analytics::input_row(txid, input.1, i))
                    }
                    Input::Lightning(i) => {
                        row(entry, &lightning::analytics::input_row(txid, input.1, i))
                    }
                }))
                .chain(tx.outputs.iter().zip(0u16..).map(|output| match output.0 {
                    Output::Ecash(o) => {
                        row(entry, &ecash::analytics::output_row(txid, output.1, o))
                    }
                    Output::Onchain(o) => row(
                        entry,
                        &onchain::analytics::output_row(
                            server.cfg.consensus.network,
                            txid,
                            output.1,
                            o,
                        ),
                    ),
                    Output::Lightning(o) => match o.as_ref() {
                        LightningOutput::Outgoing(contract) => row(
                            entry,
                            &lightning::analytics::outgoing_output_row(txid, output.1, contract),
                        ),
                        LightningOutput::Incoming(contract) => row(
                            entry,
                            &lightning::analytics::incoming_output_row(txid, output.1, contract),
                        ),
                    },
                }))
                .collect()
        }
        ConsensusItem::Module(ModuleConsensusItem::Onchain(ci)) => {
            vec![onchain::analytics::item_row(entry, ci)]
        }
        ConsensusItem::BlockHeight(height) => {
            vec![row(entry, &BlockHeightVoteRow { height: *height })]
        }
        ConsensusItem::Version(version) => {
            vec![row(entry, &VersionVoteRow { version: *version })]
        }
    }
}

/// Reads the accepted items forward: each call returns up to `limit`
/// entries past the ones returned before. Keys order by session and then
/// position, so one range scan from the cursor crosses session boundaries.
pub fn reader(db: Database) -> impl FnMut(u64) -> Vec<Entry> {
    let mut cursor = (0u32, 0u64);

    move |limit| {
        let chunk = db.begin_read().range(&AcceptedItemTable, cursor.., |r| {
            r.take(limit as usize)
                .map(|entry| Entry {
                    session: entry.0.0,
                    idx: entry.0.1,
                    node: entry.1.node,
                    item: entry.1.item,
                })
                .collect::<Vec<_>>()
        });

        if let Some(last) = chunk.last() {
            cursor = (last.session, last.idx + 1);
        }

        chunk
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every registered table installs, which is what catches a field
    /// type without a column mapping or two rows sharing a table name.
    #[test]
    fn schema_installs() {
        let sql = schema();

        println!("{sql}");

        rusqlite::Connection::open_in_memory()
            .expect("in-memory sqlite")
            .execute_batch(&sql)
            .expect("schema installs");
    }
}
