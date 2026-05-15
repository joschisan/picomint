use serde::{Deserialize, Serialize};

use picomint_encoding::{Decodable, Encodable};

/// Represents an amount of BTC. The base denomination is millisatoshis, which
/// is why the `Amount` type from rust-bitcoin isn't used instead.
#[derive(
    Debug,
    Clone,
    Copy,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    Hash,
    Deserialize,
    Serialize,
    Encodable,
    Decodable,
    Default,
)]
#[serde(transparent)]
pub struct Amount {
    // TODO: rename to `units`, with backward compat for the serialization?
    pub msat: u64,
}

impl Amount {
    pub const ZERO: Self = Self { msat: 0 };

    /// Create an amount from a number of millisatoshis.
    pub const fn from_msat(msat: u64) -> Self {
        Self { msat }
    }

    /// Create an amount from a number of satoshis.
    pub const fn from_sat(sat: u64) -> Self {
        Self::from_msat(sat * 1000)
    }

    pub fn saturating_sub(self, other: Self) -> Self {
        Self {
            msat: self.msat.saturating_sub(other.msat),
        }
    }

    pub fn mul_u64(self, other: u64) -> Self {
        Self {
            msat: self.msat * other,
        }
    }

    pub fn checked_sub(self, other: Self) -> Option<Self> {
        Some(Self {
            msat: self.msat.checked_sub(other.msat)?,
        })
    }

    pub fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            msat: self.msat.checked_add(other.msat)?,
        })
    }
}

impl std::fmt::Display for Amount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} msat", self.msat)
    }
}

impl std::ops::Rem for Amount {
    type Output = Self;

    fn rem(self, rhs: Self) -> Self::Output {
        Self {
            msat: self.msat % rhs.msat,
        }
    }
}

impl std::ops::RemAssign for Amount {
    fn rem_assign(&mut self, rhs: Self) {
        self.msat %= rhs.msat;
    }
}

impl std::ops::Div for Amount {
    type Output = u64;

    fn div(self, rhs: Self) -> Self::Output {
        self.msat / rhs.msat
    }
}

impl std::ops::SubAssign for Amount {
    fn sub_assign(&mut self, rhs: Self) {
        self.msat -= rhs.msat;
    }
}

impl std::ops::Mul<u64> for Amount {
    type Output = Self;

    fn mul(self, rhs: u64) -> Self::Output {
        Self {
            msat: self.msat * rhs,
        }
    }
}

impl std::ops::Mul<Amount> for u64 {
    type Output = Amount;

    fn mul(self, rhs: Amount) -> Self::Output {
        Amount {
            msat: self * rhs.msat,
        }
    }
}

impl std::ops::Add for Amount {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            msat: self.msat + rhs.msat,
        }
    }
}

impl std::ops::Sub for Amount {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self {
            msat: self.msat - rhs.msat,
        }
    }
}

impl std::ops::AddAssign for Amount {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl std::iter::Sum for Amount {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        Self {
            msat: iter.map(|amt| amt.msat).sum::<u64>(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Amount;

    #[test]
    fn amount_multiplication_by_scalar() {
        assert_eq!(Amount::from_msat(1000) * 123, Amount::from_msat(123_000));
    }

    #[test]
    fn scalar_multiplication_by_amount() {
        assert_eq!(123 * Amount::from_msat(1000), Amount::from_msat(123_000));
    }
}
