use crate::crypto::{PartialMultisignature, Signature};
use aleph_bft_types::{
    Index, Keychain as KeychainT, MultiKeychain as MultiKeychainT, NumPeers,
    PartialMultisignature as PartialMultisignatureT, PeerId, SignatureSet,
};

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default)]
pub struct Keychain {
    count: NumPeers,
    index: PeerId,
}

impl Keychain {
    pub fn new(count: NumPeers, index: PeerId) -> Self {
        Keychain { count, index }
    }

    pub fn new_vec(node_count: NumPeers) -> Vec<Self> {
        node_count
            .peer_ids()
            .map(|i| Self::new(node_count, i))
            .collect()
    }
}

impl Index for Keychain {
    fn index(&self) -> PeerId {
        self.index
    }
}

impl KeychainT for Keychain {
    type Signature = Signature;

    fn node_count(&self) -> NumPeers {
        self.count
    }

    fn sign(&self, msg: &[u8]) -> Self::Signature {
        Signature::new(msg.to_vec(), self.index)
    }

    fn verify(&self, msg: &[u8], sgn: &Self::Signature, index: PeerId) -> bool {
        index == sgn.index() && msg == sgn.msg()
    }
}

impl MultiKeychainT for Keychain {
    type PartialMultisignature = PartialMultisignature;

    fn bootstrap_multi(
        &self,
        signature: &Self::Signature,
        index: PeerId,
    ) -> Self::PartialMultisignature {
        SignatureSet::add_signature(SignatureSet::with_size(self.node_count()), signature, index)
    }

    fn is_complete(&self, msg: &[u8], partial: &Self::PartialMultisignature) -> bool {
        let signature_count = partial.iter().count();
        if signature_count < self.node_count().threshold() {
            return false;
        }
        partial.iter().all(|(i, sgn)| self.verify(msg, sgn, i))
    }
}
