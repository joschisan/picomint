//! How a field lands in an analytics row.
//!
//! [`SqlRow`] is derived on every row struct and turns it into its
//! table's payload columns; [`SqlColumn`] is what a field's type has to
//! implement for that, and pins the flattening rules once — an amount is
//! an integer column carrying its unit as a suffix, a hash or key is its
//! text form, a byte array is hex, an `Option` is a nullable column. The
//! derive is [`picomint_derive::SqlRow`], re-exported here, and
//! [`create_table`] renders a row type's DDL.

use bitcoin::address::NetworkUnchecked;
use bitcoin::hashes::sha256;
use bitcoin::{Address, Txid};
pub use picomint_derive::SqlRow;
use picomint_encoding::Encodable;
use secp256k1::XOnlyPublicKey;
use secp256k1::schnorr::Signature;
use tbs::BlindedNonce;

use crate::ecash::Denomination;
use crate::version::ConsensusVersion;
use crate::{Amount, NodeId, OutPoint, TransactionId};

/// A value bound into an analytics row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqlValue {
    Null,
    Integer(i64),
    Text(String),
}

/// A field type that maps to one column.
pub trait SqlColumn {
    /// SQLite storage class of the column: `INTEGER` or `TEXT`.
    const TYPE: &'static str;

    /// Appended to the storage class: every column is `NOT NULL` except
    /// an `Option`'s.
    const CONSTRAINT: &'static str = " NOT NULL";

    fn sql_value(&self) -> SqlValue;
}

/// A struct that maps to one table: its payload columns, in declaration
/// order, and its values in the same order.
pub trait SqlRow {
    /// `(name, storage class with constraint)` per column.
    fn columns() -> Vec<(String, String)>;

    fn values(&self) -> Vec<SqlValue>;
}

/// One row bound for insertion: the table it goes into and its values,
/// the table's common columns first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub table: String,
    pub values: Vec<SqlValue>,
}

/// The DDL for one table: `common` columns first, then the row type's
/// own, plus one index per entry of `indexes`, each a column list.
pub fn create_table<R: SqlRow>(table: &str, common: &[(&str, &str)], indexes: &[&str]) -> String {
    let columns = common
        .iter()
        .map(|column| format!("{} {}", column.0, column.1))
        .chain(
            R::columns()
                .iter()
                .map(|column| format!("{} {}", column.0, column.1)),
        )
        .collect::<Vec<_>>()
        .join(", ");

    let indexes = indexes
        .iter()
        .map(|columns| {
            format!(
                "CREATE INDEX {table}_{} ON {table}({columns});\n",
                columns.replace(", ", "_")
            )
        })
        .collect::<String>();

    format!("CREATE TABLE {table} ({columns});\n{indexes}")
}

macro_rules! integer {
    ($($ty:ty),*) => {
        $(impl SqlColumn for $ty {
            const TYPE: &'static str = "INTEGER";

            fn sql_value(&self) -> SqlValue {
                SqlValue::Integer(*self as i64)
            }
        })*
    };
}

integer!(u8, u16, u32, u64, i64);

macro_rules! display {
    ($($ty:ty),*) => {
        $(impl SqlColumn for $ty {
            const TYPE: &'static str = "TEXT";

            fn sql_value(&self) -> SqlValue {
                SqlValue::Text(self.to_string())
            }
        })*
    };
}

display!(
    String,
    TransactionId,
    Txid,
    OutPoint,
    Signature,
    NodeId,
    ConsensusVersion,
    XOnlyPublicKey,
    sha256::Hash
);

impl<T: SqlColumn> SqlColumn for Option<T> {
    const TYPE: &'static str = T::TYPE;

    const CONSTRAINT: &'static str = "";

    fn sql_value(&self) -> SqlValue {
        self.as_ref().map_or(SqlValue::Null, SqlColumn::sql_value)
    }
}

impl SqlColumn for bool {
    const TYPE: &'static str = "INTEGER";

    fn sql_value(&self) -> SqlValue {
        SqlValue::Integer(i64::from(*self))
    }
}

impl SqlColumn for Amount {
    const TYPE: &'static str = "INTEGER";

    fn sql_value(&self) -> SqlValue {
        SqlValue::Integer(self.msat as i64)
    }
}

impl SqlColumn for bitcoin::Amount {
    const TYPE: &'static str = "INTEGER";

    fn sql_value(&self) -> SqlValue {
        SqlValue::Integer(self.to_sat() as i64)
    }
}

impl SqlColumn for Denomination {
    const TYPE: &'static str = "INTEGER";

    fn sql_value(&self) -> SqlValue {
        SqlValue::Integer(i64::from(self.0))
    }
}

impl SqlColumn for [u8; 32] {
    const TYPE: &'static str = "TEXT";

    fn sql_value(&self) -> SqlValue {
        SqlValue::Text(hex::encode(self))
    }
}

impl SqlColumn for BlindedNonce {
    const TYPE: &'static str = "TEXT";

    fn sql_value(&self) -> SqlValue {
        SqlValue::Text(hex::encode(self.consensus_encode_to_vec()))
    }
}

impl SqlColumn for Address<NetworkUnchecked> {
    const TYPE: &'static str = "TEXT";

    fn sql_value(&self) -> SqlValue {
        SqlValue::Text(self.assume_checked_ref().to_string())
    }
}
