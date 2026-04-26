use crate::keychain::{add_partial_signature, Keychain, PartialMultisignature, Signature};
use crate::node::Index;
use crate::PeerId;
use log::warn;
use picomint_encoding::{Decodable, Encodable};

/// Data which can be signed.
///
/// Signable data should provide a hash of type [`Self::Hash`] which is built from all parts of the
/// data which should be signed. The type [`Self::Hash`] should implement [`AsRef<[u8]>`], and
/// the bytes returned by `hash.as_ref()` are used by a [`Keychain`] to sign the data.
pub trait Signable: Encodable + Decodable {
    type Hash: AsRef<[u8]>;
    /// Return a hash for signing.
    fn hash(&self) -> Self::Hash;
}

impl<T: AsRef<[u8]> + Clone + Encodable + Decodable> Signable for T {
    type Hash = T;
    fn hash(&self) -> Self::Hash {
        self.clone()
    }
}

/// A pair consisting of an instance of the `Signable` trait and an (arbitrary) signature.
///
/// The method [`UncheckedSigned::check`] can be used to upgrade this `struct` to
/// [`Signed<T>`] which ensures that the signature matches the signed object.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Decodable, Encodable)]
pub struct UncheckedSigned<T: Signable, S: Encodable + Decodable> {
    signable: T,
    signature: S,
}

impl<T: Signable, S: Encodable + Decodable + Clone> UncheckedSigned<T, S> {
    pub fn as_signable(&self) -> &T {
        &self.signable
    }

    pub fn into_signable(self) -> T {
        self.signable
    }

    pub fn signature(&self) -> S {
        self.signature.clone()
    }
}

impl<T: Signable, S: Encodable + Decodable> UncheckedSigned<Indexed<T>, S> {
    pub fn as_signable_strip_index(&self) -> &T {
        &self.signable.signable
    }
}

/// Error type returned when a verification of a signature fails.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Decodable, Encodable)]
pub struct SignatureError<T: Signable, S: Encodable + Decodable> {
    pub unchecked: UncheckedSigned<T, S>,
}

impl<T: Signable + Index> UncheckedSigned<T, Signature> {
    /// Verifies whether the signature matches the key with the index as in the signed data.
    pub fn check(self, keychain: &Keychain) -> Result<Signed<T>, SignatureError<T, Signature>> {
        let index = self.signable.index();
        if !keychain.verify(self.signable.hash().as_ref(), &self.signature, index) {
            return Err(SignatureError { unchecked: self });
        }
        Ok(Signed { unchecked: self })
    }
}

impl<T: Signable + Index, S: Encodable + Decodable> Index for UncheckedSigned<T, S> {
    fn index(&self) -> PeerId {
        self.signable.index()
    }
}

impl<T: Signable> UncheckedSigned<T, PartialMultisignature> {
    /// Verifies whether the multisignature matches the signed data.
    pub fn check_multi(
        self,
        keychain: &Keychain,
    ) -> Result<Multisigned<T>, SignatureError<T, PartialMultisignature>> {
        if !keychain.is_complete(self.signable.hash().as_ref(), &self.signature) {
            return Err(SignatureError { unchecked: self });
        }
        Ok(Multisigned { unchecked: self })
    }
}

impl<T: Signable, S: Encodable + Decodable> UncheckedSigned<Indexed<T>, S> {
    fn strip_index(self) -> UncheckedSigned<T, S> {
        UncheckedSigned {
            signable: self.signable.strip_index(),
            signature: self.signature,
        }
    }
}

impl<T: Signable, S: Encodable + Decodable> From<UncheckedSigned<Indexed<T>, S>>
    for UncheckedSigned<T, S>
{
    fn from(us: UncheckedSigned<Indexed<T>, S>) -> Self {
        us.strip_index()
    }
}

/// A correctly signed object of type `T`.
#[derive(Eq, PartialEq, Hash, Debug, Decodable, Encodable)]
pub struct Signed<T: Signable + Index> {
    unchecked: UncheckedSigned<T, Signature>,
}

impl<T: Signable + Clone + Index> Clone for Signed<T> {
    fn clone(&self) -> Self {
        Signed {
            unchecked: self.unchecked.clone(),
        }
    }
}

impl<T: Signable + Index> Signed<T> {
    /// Create a signed object from a signable. The index of `signable` must match the index of the `keychain`.
    pub fn sign(signable: T, keychain: &Keychain) -> Signed<T> {
        assert_eq!(signable.index(), keychain.index());
        let signature = keychain.sign(signable.hash().as_ref());
        Signed {
            unchecked: UncheckedSigned {
                signable,
                signature,
            },
        }
    }

    /// Get a reference to the signed object.
    pub fn as_signable(&self) -> &T {
        &self.unchecked.signable
    }

    pub fn into_signable(self) -> T {
        self.unchecked.signable
    }

    pub fn into_unchecked(self) -> UncheckedSigned<T, Signature> {
        self.unchecked
    }
}

impl<T: Signable> Signed<Indexed<T>> {
    /// Create a signed object from a signable. The index is added based on the index of the `keychain`.
    pub fn sign_with_index(signable: T, keychain: &Keychain) -> Signed<Indexed<T>> {
        Signed::sign(Indexed::new(signable, keychain.index()), keychain)
    }

    /// Transform a singly signed object into a partially multisigned consisting of just the signed object.
    /// Note that depending on the setup, it may yield a complete signature.
    pub fn into_partially_multisigned(self, keychain: &Keychain) -> PartiallyMultisigned<T> {
        let multisignature =
            keychain.bootstrap_multi(&self.unchecked.signature, self.unchecked.signable.index);
        let unchecked = UncheckedSigned {
            signable: self.unchecked.signable.strip_index(),
            signature: multisignature,
        };
        if keychain.is_complete(unchecked.signable.hash().as_ref(), &unchecked.signature) {
            PartiallyMultisigned::Complete {
                multisigned: Multisigned { unchecked },
            }
        } else {
            PartiallyMultisigned::Incomplete { unchecked }
        }
    }
}

impl<T: Signable + Index> From<Signed<T>> for UncheckedSigned<T, Signature> {
    fn from(signed: Signed<T>) -> Self {
        signed.into_unchecked()
    }
}

/// A pair consisting of signable data and a [`PeerId`].
///
/// This is a wrapper used for signing data which does not implement the [`Index`] trait.
/// If a node with an index `i` needs to sign some data `signable` which does not
/// implement the [`Index`] trait, it should use the `Signed::sign_with_index` method which will
/// use this wrapper transparently. Note that in the implementation of `Signable` for `Indexed<T>`,
/// the hash is the hash of the underlying data `T`. Therefore, instances of the type
/// [`Signed<Indexed<T>>`] can be aggregated into [`Multisigned<T>`].
#[derive(Clone, Eq, PartialEq, Hash, Debug, Decodable, Encodable)]
pub struct Indexed<T: Signable> {
    signable: T,
    index: PeerId,
}

impl<T: Signable> Indexed<T> {
    fn new(signable: T, index: PeerId) -> Self {
        Indexed { signable, index }
    }

    fn strip_index(self) -> T {
        self.signable
    }

    pub fn as_signable(&self) -> &T {
        &self.signable
    }
}

impl<T: Signable> Signable for Indexed<T> {
    type Hash = T::Hash;

    fn hash(&self) -> Self::Hash {
        self.signable.hash()
    }
}

impl<T: Signable> Index for Indexed<T> {
    fn index(&self) -> PeerId {
        self.index
    }
}

/// Signable data together with a complete multisignature.
#[derive(Eq, PartialEq, Hash, Debug, Decodable, Encodable)]
pub struct Multisigned<T: Signable> {
    unchecked: UncheckedSigned<T, PartialMultisignature>,
}

impl<T: Signable> Multisigned<T> {
    /// Get a reference to the multisigned object.
    pub fn as_signable(&self) -> &T {
        &self.unchecked.signable
    }

    pub fn into_unchecked(self) -> UncheckedSigned<T, PartialMultisignature> {
        self.unchecked
    }
}

impl<T: Signable> From<Multisigned<T>> for UncheckedSigned<T, PartialMultisignature> {
    fn from(signed: Multisigned<T>) -> Self {
        signed.into_unchecked()
    }
}

impl<T: Signable + Clone> Clone for Multisigned<T> {
    fn clone(&self) -> Self {
        Multisigned {
            unchecked: self.unchecked.clone(),
        }
    }
}

/// Error resulting from multisignature being incomplete.
#[derive(Clone, Eq, PartialEq, Debug, Decodable, Encodable)]
pub struct IncompleteMultisignatureError<T: Signable> {
    pub partial: PartiallyMultisigned<T>,
}

/// Signable data together with a valid partial multisignature.
///
/// Instances of this type keep track whether the partial multisignature is complete or not.
/// If the multisignature is complete, you can get [`Multisigned`] by pattern matching
/// against the variant [`PartiallyMultisigned::Complete`].
#[derive(Clone, Eq, PartialEq, Hash, Debug, Decodable, Encodable)]
pub enum PartiallyMultisigned<T: Signable> {
    Incomplete {
        unchecked: UncheckedSigned<T, PartialMultisignature>,
    },
    Complete {
        multisigned: Multisigned<T>,
    },
}

impl<T: Signable> PartiallyMultisigned<T> {
    /// Create a partially multisigned object.
    pub fn sign(signable: T, keychain: &Keychain) -> PartiallyMultisigned<T> {
        Signed::sign_with_index(signable, keychain).into_partially_multisigned(keychain)
    }

    /// Check if the partial multisignature is complete.
    pub fn is_complete(&self) -> bool {
        match self {
            PartiallyMultisigned::Incomplete { .. } => false,
            PartiallyMultisigned::Complete { .. } => true,
        }
    }

    /// Get a reference to the multisigned object.
    pub fn as_signable(&self) -> &T {
        match self {
            PartiallyMultisigned::Incomplete { unchecked } => unchecked.as_signable(),
            PartiallyMultisigned::Complete { multisigned } => multisigned.as_signable(),
        }
    }

    /// Return the object that is being signed.
    pub fn into_unchecked(self) -> UncheckedSigned<T, PartialMultisignature> {
        match self {
            PartiallyMultisigned::Incomplete { unchecked } => unchecked,
            PartiallyMultisigned::Complete { multisigned } => multisigned.unchecked,
        }
    }

    /// Adds a signature and checks if multisignature is complete.
    #[must_use = "consumes the original and returns the aggregated signature which should be used"]
    pub fn add_signature(self, signed: Signed<Indexed<T>>, keychain: &Keychain) -> Self {
        if self.as_signable().hash().as_ref() != signed.as_signable().hash().as_ref() {
            warn!(target: "AlephBFT-signed", "Tried to add a signature of a different object");
            return self;
        }
        match self {
            PartiallyMultisigned::Incomplete { mut unchecked } => {
                unchecked.signature = add_partial_signature(
                    unchecked.signature,
                    &signed.unchecked.signature,
                    signed.unchecked.signable.index,
                );
                if keychain.is_complete(unchecked.signable.hash().as_ref(), &unchecked.signature) {
                    PartiallyMultisigned::Complete {
                        multisigned: Multisigned { unchecked },
                    }
                } else {
                    PartiallyMultisigned::Incomplete { unchecked }
                }
            }
            PartiallyMultisigned::Complete { .. } => self,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use picomint_core::secp256k1::{PublicKey, SecretKey, SECP256K1};
    use picomint_encoding::{Decodable, Encodable};

    use crate::{
        Indexed, Keychain, NumPeers, PartiallyMultisigned, PeerId, Signable, Signed,
        UncheckedSigned,
    };

    #[derive(Clone, Debug, Default, PartialEq, Eq, Encodable, Decodable)]
    struct TestMessage {
        msg: Vec<u8>,
    }

    impl Signable for TestMessage {
        type Hash = Vec<u8>;
        fn hash(&self) -> Self::Hash {
            self.msg.clone()
        }
    }

    fn test_message() -> TestMessage {
        TestMessage {
            msg: "Hello".as_bytes().to_vec(),
        }
    }

    fn deterministic_secret(seed: u8) -> SecretKey {
        let mut bytes = [1u8; 32];
        bytes[31] = seed.saturating_add(1);
        SecretKey::from_slice(&bytes).expect("non-zero 32-byte slice is a valid SecretKey")
    }

    fn keychains(node_count: NumPeers) -> Vec<Keychain> {
        let secrets: Vec<SecretKey> = node_count
            .peer_ids()
            .map(|p| deterministic_secret(p.to_usize() as u8))
            .collect();
        let public_keys: BTreeMap<PeerId, PublicKey> = node_count
            .peer_ids()
            .zip(secrets.iter())
            .map(|(p, s)| (p, PublicKey::from_secret_key(SECP256K1, s)))
            .collect();
        secrets
            .into_iter()
            .enumerate()
            .map(|(i, s)| Keychain::new(public_keys.clone(), PeerId::from(i as u8), s))
            .collect()
    }

    #[test]
    fn test_valid_signatures() {
        let node_count: NumPeers = 7.into();
        let keychains = keychains(node_count);
        for i in 0..node_count.total() {
            for j in 0..node_count.total() {
                let msg = test_message();
                let signed_msg = Signed::sign_with_index(msg.clone(), &keychains[i]);
                let unchecked_msg = signed_msg.into_unchecked();
                let unchecked: UncheckedSigned<Indexed<TestMessage>, _> = unchecked_msg;
                assert!(
                    unchecked.check(&keychains[j]).is_ok(),
                    "Signed message should be valid"
                );
            }
        }
    }

    #[test]
    fn test_invalid_signatures() {
        let node_count: NumPeers = 1.into();
        let mut chains = keychains(node_count);
        let keychain = chains.pop().unwrap();
        let msg = test_message();
        let signed_msg = Signed::sign_with_index(msg, &keychain);
        let mut unchecked_msg = signed_msg.into_unchecked();
        // Flip a byte in the signature to make it invalid.
        unchecked_msg.signature[0] ^= 0xFF;
        assert!(
            unchecked_msg.check(&keychain).is_err(),
            "tampered signature should not validate"
        );
    }

    #[test]
    fn test_incomplete_multisignature() {
        let msg = test_message();
        let mut chains = keychains(2.into());
        let keychain = chains.pop().unwrap();
        let partial = PartiallyMultisigned::sign(msg, &keychain);
        assert!(
            !partial.is_complete(),
            "One signature does not form a complete multisignature",
        );
    }

    #[test]
    fn test_multisignatures() {
        let msg = test_message();
        let node_count: NumPeers = 7.into();
        let chains = keychains(node_count);
        let mut partial = PartiallyMultisigned::sign(msg.clone(), &chains[0]);
        for keychain in chains.iter().skip(1).take(4) {
            assert!(!partial.is_complete());
            let signed = Signed::sign_with_index(msg.clone(), keychain);
            partial = partial.add_signature(signed, keychain);
        }
        assert!(
            partial.is_complete(),
            "5 signatures should form a complete signature {:?}",
            partial
        );
    }

    // Compile-time assertion that NodeMap<Signature> can be constructed.
    #[allow(dead_code)]
    fn _compile_check_node_map_signature() -> crate::NodeMap<crate::Signature> {
        crate::NodeMap::with_size(NumPeers::from(3))
    }
}
