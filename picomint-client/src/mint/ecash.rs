use std::fmt;
use std::str::FromStr;

use picomint_core::Amount;
use picomint_core::config::FederationId;
use picomint_encoding::{Decodable, Encodable};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::SpendableNote;

/// Out-of-band Chaumian ecash bundle. The serde representation is the
/// `picomint`-prefixed base32 string that callers hand off — so events
/// carrying an `ECash` log it the same way it travels on the wire.
#[derive(Clone, Debug, Encodable, Decodable)]
pub struct ECash {
    pub mint: FederationId,
    pub notes: Vec<SpendableNote>,
}

impl ECash {
    pub fn new(mint: FederationId, notes: Vec<SpendableNote>) -> Self {
        Self { mint, notes }
    }

    pub fn amount(&self) -> Amount {
        self.notes.iter().map(SpendableNote::amount).sum()
    }
}

impl fmt::Display for ECash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&picomint_base32::encode(self))
    }
}

impl FromStr for ECash {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        picomint_base32::decode(s)
    }
}

impl Serialize for ECash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        picomint_base32::encode(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ECash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        picomint_base32::decode(&String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}
