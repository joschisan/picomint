use crate::{
    units::{FullUnit, PreUnit, SignedUnit},
    Data, Keychain, SessionId, Signed,
};

/// The component responsible for packing Data into PreUnits,
/// and signing the outcome, thus creating SignedUnits that are sent back to consensus.
pub struct Packer {
    keychain: Keychain,
    session_id: SessionId,
}

impl Packer {
    pub fn new(keychain: Keychain, session_id: SessionId) -> Self {
        Packer {
            keychain,
            session_id,
        }
    }

    pub fn pack<D: Data>(&self, preunit: PreUnit, data: Option<D>) -> SignedUnit<D> {
        Signed::sign(
            FullUnit::new(preunit, data, self.session_id),
            &self.keychain,
        )
    }
}
