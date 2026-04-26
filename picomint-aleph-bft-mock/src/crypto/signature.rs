use aleph_bft_types::{Index, PeerId, SignatureSet};
use picomint_encoding::{Decodable, Encodable};
use std::hash::Hash;

#[derive(Clone, Eq, PartialEq, Hash, Debug, Default, Encodable, Decodable)]
pub struct Signature {
    msg: Vec<u8>,
    index: PeerId,
}

impl Signature {
    pub fn new(msg: Vec<u8>, index: PeerId) -> Self {
        Self { msg, index }
    }

    pub fn msg(&self) -> &Vec<u8> {
        &self.msg
    }
}

impl Index for Signature {
    fn index(&self) -> PeerId {
        self.index
    }
}

pub type PartialMultisignature = SignatureSet<Signature>;
