use crate::Amount;
use picomint_encoding::{Decodable, Encodable};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize, Encodable, Decodable)]
pub struct LightningConfigConsensus {
    pub input_fee: Amount,
    pub output_fee: Amount,
}
