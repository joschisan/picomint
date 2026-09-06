//! How an event field lands in an analytics row.
//!
//! [`SqlRow`] is derived on every event struct and turns it into its
//! table's payload columns; [`SqlColumn`] is what a field's type has to
//! implement for that, and pins the flattening rules once — an amount is
//! an integer column carrying its unit as a suffix, a hash or key is its
//! text form, a byte array is hex. The derive is
//! [`picomint_derive::SqlRow`], re-exported here.

use bitcoin::address::NetworkUnchecked;
use bitcoin::{Address, Txid};
pub use picomint_derive::SqlRow;
use secp256k1::schnorr::Signature;

use crate::{Amount, OutPoint, TransactionId};

/// A value bound into an analytics row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqlValue {
    Integer(i64),
    Text(String),
}

/// A field type that maps to one column.
pub trait SqlColumn {
    /// SQLite storage class of the column: `INTEGER` or `TEXT`.
    const TYPE: &'static str;

    fn sql_value(&self) -> SqlValue;
}

/// A struct that maps to one table: its payload columns, in declaration
/// order, and its values in the same order.
pub trait SqlRow {
    /// `(name, storage class)` per column.
    fn columns() -> Vec<(String, &'static str)>;

    fn values(&self) -> Vec<SqlValue>;
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

display!(String, TransactionId, Txid, OutPoint, Signature);

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

impl SqlColumn for [u8; 32] {
    const TYPE: &'static str = "TEXT";

    fn sql_value(&self) -> SqlValue {
        SqlValue::Text(hex::encode(self))
    }
}

impl SqlColumn for Address<NetworkUnchecked> {
    const TYPE: &'static str = "TEXT";

    fn sql_value(&self) -> SqlValue {
        SqlValue::Text(self.assume_checked_ref().to_string())
    }
}
