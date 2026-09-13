use iroh_base::PublicKey;

use crate::config::MintId;
use picomint_encoding::{Base32, Decodable, Encodable};

/// An invite code: the mint id, the iroh public key of the node that issued
/// it and the invite id that node enforces the code's expiry and user limit
/// against when serving the mint config. A client hands it to `add` to join
/// the mint; the config it downloads is checked against the mint id.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Encodable, Decodable, Base32)]
pub struct InviteCode {
    pub mint: MintId,
    pub iroh_pk: PublicKey,
    pub invite_id: [u8; 16],
}

impl InviteCode {
    pub fn new(iroh_pk: PublicKey, mint: MintId, invite_id: [u8; 16]) -> Self {
        Self {
            mint,
            iroh_pk,
            invite_id,
        }
    }
}
