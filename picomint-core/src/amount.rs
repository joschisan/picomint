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
    pub msats: u64,
}

impl Amount {
    pub const ZERO: Self = Self { msats: 0 };

    /// Create an amount from a number of millisatoshis.
    pub const fn from_msats(msats: u64) -> Self {
        Self { msats }
    }

    /// Create an amount from a number of satoshis.
    pub const fn from_sats(sats: u64) -> Self {
        Self::from_msats(sats * 1000)
    }

    pub fn saturating_sub(self, other: Self) -> Self {
        Self {
            msats: self.msats.saturating_sub(other.msats),
        }
    }

    pub fn mul_u64(self, other: u64) -> Self {
        Self {
            msats: self.msats * other,
        }
    }

    pub fn checked_sub(self, other: Self) -> Option<Self> {
        Some(Self {
            msats: self.msats.checked_sub(other.msats)?,
        })
    }

    pub fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            msats: self.msats.checked_add(other.msats)?,
        })
    }
}

impl std::fmt::Display for Amount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} msat", self.msats)
    }
}

impl std::ops::Rem for Amount {
    type Output = Self;

    fn rem(self, rhs: Self) -> Self::Output {
        Self {
            msats: self.msats % rhs.msats,
        }
    }
}

impl std::ops::RemAssign for Amount {
    fn rem_assign(&mut self, rhs: Self) {
        self.msats %= rhs.msats;
    }
}

impl std::ops::Div for Amount {
    type Output = u64;

    fn div(self, rhs: Self) -> Self::Output {
        self.msats / rhs.msats
    }
}

impl std::ops::SubAssign for Amount {
    fn sub_assign(&mut self, rhs: Self) {
        self.msats -= rhs.msats;
    }
}

impl std::ops::Mul<u64> for Amount {
    type Output = Self;

    fn mul(self, rhs: u64) -> Self::Output {
        Self {
            msats: self.msats * rhs,
        }
    }
}

impl std::ops::Mul<Amount> for u64 {
    type Output = Amount;

    fn mul(self, rhs: Amount) -> Self::Output {
        Amount {
            msats: self * rhs.msats,
        }
    }
}

impl std::ops::Add for Amount {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            msats: self.msats + rhs.msats,
        }
    }
}

impl std::ops::Sub for Amount {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self {
            msats: self.msats - rhs.msats,
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
            msats: iter.map(|amt| amt.msats).sum::<u64>(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Amount;

    #[test]
    fn amount_multiplication_by_scalar() {
        assert_eq!(Amount::from_msats(1000) * 123, Amount::from_msats(123_000));
    }

    #[test]
    fn scalar_multiplication_by_amount() {
        assert_eq!(123 * Amount::from_msats(1000), Amount::from_msats(123_000));
    }
}
