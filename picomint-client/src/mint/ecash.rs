use picomint_core::Amount;
use picomint_core::config::FederationId;
use picomint_encoding::{Decodable, Encodable};

use super::SpendableNote;

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
