use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use picomint_encoding::{Decodable, Encodable};

/// An amount in millisatoshi, which is why the `Amount` type from
/// rust-bitcoin isn't used instead. Serializes as the bare msat integer.
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
    JsonSchema,
)]
#[serde(transparent)]
pub struct Amount(pub u64);

impl Amount {
    pub const ZERO: Self = Self(0);

    /// Create an amount from a number of satoshis.
    pub const fn from_sat(sat: u64) -> Self {
        Self(sat * 1000)
    }

    pub fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }

    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Self)
    }

    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }
}

impl std::fmt::Display for Amount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} msat", self.0)
    }
}

impl std::ops::Rem for Amount {
    type Output = Self;

    fn rem(self, rhs: Self) -> Self::Output {
        Self(self.0 % rhs.0)
    }
}

impl std::ops::RemAssign for Amount {
    fn rem_assign(&mut self, rhs: Self) {
        self.0 %= rhs.0;
    }
}

impl std::ops::Div for Amount {
    type Output = u64;

    fn div(self, rhs: Self) -> Self::Output {
        self.0 / rhs.0
    }
}

impl std::ops::SubAssign for Amount {
    fn sub_assign(&mut self, rhs: Self) {
        self.0 -= rhs.0;
    }
}

impl std::ops::Mul<u64> for Amount {
    type Output = Self;

    fn mul(self, rhs: u64) -> Self::Output {
        Self(self.0 * rhs)
    }
}

impl std::ops::Mul<Amount> for u64 {
    type Output = Amount;

    fn mul(self, rhs: Amount) -> Self::Output {
        Amount(self * rhs.0)
    }
}

impl std::ops::Add for Amount {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl std::ops::Sub for Amount {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0 - rhs.0)
    }
}

impl std::ops::AddAssign for Amount {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl std::iter::Sum for Amount {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        Self(iter.map(|amount| amount.0).sum())
    }
}

#[cfg(test)]
mod tests {
    use super::Amount;

    #[test]
    fn amount_multiplication_by_scalar() {
        assert_eq!(Amount(1000) * 123, Amount(123_000));
    }

    #[test]
    fn scalar_multiplication_by_amount() {
        assert_eq!(123 * Amount(1000), Amount(123_000));
    }
}
