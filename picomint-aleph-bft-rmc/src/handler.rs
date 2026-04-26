//! Reliable MultiCast - a primitive for Reliable Broadcast protocol.
pub use aleph_bft_crypto::{
    Indexed, Keychain, Multisigned, PartialMultisignature, PartiallyMultisigned, Signable,
    Signature, Signed, UncheckedSigned,
};
use core::fmt::Debug;
use std::{
    collections::HashMap,
    fmt::{Display, Formatter},
    hash::Hash,
};

#[derive(Debug, PartialEq)]
pub enum Error {
    BadSignature,
    BadMultisignature,
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::BadSignature => write!(f, "received a hash with a bad signature."),
            Error::BadMultisignature => write!(f, "received a hash with a bad multisignature."),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum OnStartRmcResponse<H: Signable> {
    SignedHash(Signed<Indexed<H>>),
    MultisignedHash(Multisigned<H>),
    Noop,
}

pub struct Handler<H: Signable + Hash> {
    keychain: Keychain,
    hash_states: HashMap<H, PartiallyMultisigned<H>>,
}

impl<H: Signable + Hash + Eq + Clone + Debug> Handler<H> {
    pub fn new(keychain: Keychain) -> Self {
        Handler {
            hash_states: HashMap::new(),
            keychain,
        }
    }

    /// Signs hash and updates the internal state with it. Returns the signed
    /// version of the hash for broadcast. Should be called at most once for a particular hash.
    pub fn on_start_rmc(&mut self, hash: H) -> OnStartRmcResponse<H> {
        let signed_hash = Signed::sign_with_index(hash, &self.keychain);
        if self.already_completed(signed_hash.as_signable().as_signable()) {
            return OnStartRmcResponse::Noop;
        }
        if let Some(multisigned) = self.handle_signed_hash(signed_hash.clone()) {
            return OnStartRmcResponse::MultisignedHash(multisigned);
        }
        OnStartRmcResponse::SignedHash(signed_hash)
    }

    /// Update the internal state with the signed hash. If the hash is incorrectly signed then
    /// [`Error::BadSignature`] is returned. If Adding this signature completes a multisignature
    /// then `Ok(multisigned)` is returned. Otherwise `Ok(None)` is returned.
    pub fn on_signed_hash(
        &mut self,
        unchecked: UncheckedSigned<Indexed<H>, Signature>,
    ) -> Result<Option<Multisigned<H>>, Error> {
        let signed_hash = unchecked
            .check(&self.keychain)
            .map_err(|_| Error::BadSignature)?;
        Ok(
            match self.already_completed(signed_hash.as_signable().as_signable()) {
                true => None,
                false => self.handle_signed_hash(signed_hash),
            },
        )
    }

    fn handle_signed_hash(&mut self, signed: Signed<Indexed<H>>) -> Option<Multisigned<H>> {
        let hash = signed.as_signable().as_signable().clone();
        let new_state = match self.hash_states.remove(&hash) {
            None => signed.into_partially_multisigned(&self.keychain),
            Some(partial) => partial.add_signature(signed, &self.keychain),
        };
        match new_state {
            PartiallyMultisigned::Complete { multisigned } => {
                self.hash_states.insert(
                    hash,
                    PartiallyMultisigned::Complete {
                        multisigned: multisigned.clone(),
                    },
                );
                Some(multisigned)
            }
            incomplete => {
                self.hash_states.insert(hash, incomplete);
                None
            }
        }
    }

    /// Update the internal state with the finished multisigned hash. If the hash is incorrectly
    /// signed then [`Error::BadMultisignature`] is returned. Otherwise `multisigned` is returned,
    /// unless the multisignature got completed earlier.
    pub fn on_multisigned_hash(
        &mut self,
        unchecked: UncheckedSigned<H, PartialMultisignature>,
    ) -> Result<Option<Multisigned<H>>, Error> {
        if self.already_completed(unchecked.as_signable()) {
            return Ok(None);
        }

        let multisigned = unchecked
            .check_multi(&self.keychain)
            .map_err(|_| Error::BadMultisignature)?;
        self.hash_states.insert(
            multisigned.as_signable().clone(),
            PartiallyMultisigned::Complete {
                multisigned: multisigned.clone(),
            },
        );
        Ok(Some(multisigned))
    }

    fn already_completed(&self, hash: &H) -> bool {
        matches!(
            self.hash_states.get(hash),
            Some(PartiallyMultisigned::Complete { .. })
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        handler::{Error, OnStartRmcResponse},
        Handler,
    };
    use aleph_bft_crypto::{NumPeers, PartiallyMultisigned, Signed};
    use aleph_bft_mock::{bad_keychain, keychain, Signable};

    fn apply_signatures(
        handler: &mut Handler<Signable>,
        hash: &Signable,
        keychains: impl Iterator<Item = aleph_bft_crypto::Keychain>,
    ) {
        for kc in keychains {
            let signed_hash = Signed::sign_with_index(hash.clone(), &kc);
            handler
                .on_signed_hash(signed_hash.into_unchecked())
                .expect("the signatures should be correct");
        }
    }

    fn apply_signatures_and_get_multisigned(
        handler: &mut Handler<Signable>,
        hash: &Signable,
        keychains: impl Iterator<Item = aleph_bft_crypto::Keychain>,
    ) -> Option<PartiallyMultisigned<Signable>> {
        let mut multisigned = None;
        for kc in keychains {
            let signed_hash = Signed::sign_with_index(hash.clone(), &kc);
            handler
                .on_signed_hash(signed_hash.clone().into_unchecked())
                .expect("the signatures should be correct");
            multisigned = match multisigned {
                None => Some(signed_hash.into_partially_multisigned(&kc)),
                Some(ms) => Some(ms.add_signature(signed_hash, &kc)),
            }
        }
        multisigned
    }

    #[test]
    fn on_start_rmc_before_reaching_quorum_returns_signed() {
        let hash: Signable = "13".into();
        let kc = keychain(7.into(), 0.into());
        let mut handler = Handler::new(kc.clone());
        match handler.on_start_rmc(hash.clone()) {
            OnStartRmcResponse::SignedHash(signed) => {
                assert_eq!(signed.as_signable().as_signable(), &hash);
            }
            other => panic!("expected SignedHash, got {:?}", other),
        }
    }

    #[test]
    fn on_start_rmc_reaching_quorum_returns_multisigned() {
        let hash: Signable = "13".into();
        let kc = keychain(7.into(), 0.into());
        let mut handler = Handler::new(kc.clone());
        let multisigned = apply_signatures_and_get_multisigned(
            &mut handler,
            &hash,
            (1..5).map(|i| keychain(7.into(), PeerId::from(i as u8))),
        )
        .expect("passed nodes set is non-empty");
        let multisigned =
            multisigned.add_signature(Signed::sign_with_index(hash.clone(), &kc), &kc);
        match multisigned {
            PartiallyMultisigned::Incomplete { .. } => panic!("multisignature should be complete"),
            PartiallyMultisigned::Complete { multisigned } => {
                match handler.on_start_rmc(hash.clone()) {
                    OnStartRmcResponse::MultisignedHash(actual) => {
                        assert_eq!(actual.as_signable(), multisigned.as_signable());
                    }
                    other => panic!("expected MultisignedHash, got {:?}", other),
                }
            }
        }
    }

    #[test]
    fn on_start_rmc_after_reaching_quorum_returns_noop() {
        let hash: Signable = "13".into();
        let kc = keychain(7.into(), 0.into());
        let mut handler = Handler::new(kc);
        apply_signatures(
            &mut handler,
            &hash,
            (1..6).map(|i| keychain(7.into(), PeerId::from(i as u8))),
        );
        assert_eq!(handler.on_start_rmc(hash), OnStartRmcResponse::Noop);
    }

    #[test]
    fn on_signed_hash_before_reaching_quorum_returns_none() {
        let hash: Signable = "13".into();
        let kc = keychain(7.into(), 0.into());
        let mut handler = Handler::new(kc);
        let peer_kc = keychain(7.into(), 1.into());
        let peer_signed = Signed::sign_with_index(hash, &peer_kc);
        assert_eq!(
            handler.on_signed_hash(peer_signed.into_unchecked()),
            Ok(None)
        );
    }

    #[test]
    fn on_signed_hash_reaching_quorum_returns_multisigned() {
        let hash: Signable = "13".into();
        let kc = keychain(7.into(), 0.into());
        let mut handler = Handler::new(kc);
        let peer_kc = keychain(7.into(), 1.into());
        let multisigned = apply_signatures_and_get_multisigned(
            &mut handler,
            &hash,
            (2..6).map(|i| keychain(7.into(), PeerId::from(i as u8))),
        )
        .expect("passed nodes set is non-empty");
        let peer_signed = Signed::sign_with_index(hash, &peer_kc);
        let multisigned = multisigned.add_signature(peer_signed.clone(), &peer_kc);
        match multisigned {
            PartiallyMultisigned::Incomplete { .. } => panic!("multisignature should be complete"),
            PartiallyMultisigned::Complete { multisigned } => {
                match handler.on_signed_hash(peer_signed.into_unchecked()) {
                    Ok(Some(actual)) => {
                        assert_eq!(actual.as_signable(), multisigned.as_signable());
                    }
                    other => panic!("expected Some(multisigned), got {:?}", other),
                }
            }
        }
    }

    #[test]
    fn on_signed_hash_after_reaching_quorum_returns_none() {
        let hash: Signable = "13".into();
        let kc = keychain(7.into(), 0.into());
        let mut handler = Handler::new(kc.clone());
        apply_signatures(
            &mut handler,
            &hash,
            (1..6).map(|i| keychain(7.into(), PeerId::from(i as u8))),
        );
        let our_signed = Signed::sign_with_index(hash, &kc);
        assert_eq!(
            handler.on_signed_hash(our_signed.into_unchecked()),
            Ok(None)
        );
    }

    #[test]
    fn on_signed_hash_with_bad_signature_fails() {
        let hash: Signable = "13".into();
        let kc = keychain(7.into(), 0.into());
        let mut handler = Handler::new(kc);
        let bad_kc = bad_keychain(7.into(), 1.into());
        let bad_signed = Signed::sign_with_index(hash, &bad_kc);
        assert_eq!(
            handler.on_signed_hash(bad_signed.into_unchecked()),
            Err(Error::BadSignature)
        );
    }

    #[test]
    fn on_multisigned_hash_with_new_multisigned_returns_multisigned() {
        let hash: Signable = "13".into();
        let kc = keychain(7.into(), 0.into());
        let mut handler = Handler::new(kc);
        let peer_kc = keychain(7.into(), 1.into());
        let mut peer_handler = Handler::new(peer_kc);
        let multisigned = apply_signatures_and_get_multisigned(
            &mut peer_handler,
            &hash,
            (1..6).map(|i| keychain(7.into(), PeerId::from(i as u8))),
        )
        .expect("passed nodes set is non-empty");
        match multisigned {
            PartiallyMultisigned::Incomplete { .. } => panic!("multisignature should be complete"),
            PartiallyMultisigned::Complete { multisigned } => {
                let cloned = multisigned.clone();
                match handler.on_multisigned_hash(multisigned.into_unchecked()) {
                    Ok(Some(actual)) => {
                        assert_eq!(actual.as_signable(), cloned.as_signable());
                    }
                    other => panic!("expected Some, got {:?}", other),
                }
            }
        }
    }

    #[test]
    fn on_multisigned_hash_with_known_multisigned_returns_none() {
        let hash: Signable = "13".into();
        let kc = keychain(7.into(), 0.into());
        let mut handler = Handler::new(kc);
        let multisigned = apply_signatures_and_get_multisigned(
            &mut handler,
            &hash,
            (1..6).map(|i| keychain(7.into(), PeerId::from(i as u8))),
        )
        .expect("passed nodes set is non-empty");
        match multisigned {
            PartiallyMultisigned::Incomplete { .. } => panic!("multisignature should be complete"),
            PartiallyMultisigned::Complete { multisigned } => assert_eq!(
                handler.on_multisigned_hash(multisigned.into_unchecked()),
                Ok(None)
            ),
        }
    }

    #[test]
    fn on_multisigned_hash_with_bad_multisignature_fails() {
        let hash: Signable = "13".into();
        let kc = keychain(7.into(), 0.into());
        let mut handler = Handler::new(kc);
        let multisigned = apply_signatures_and_get_multisigned(
            &mut handler,
            &hash,
            (1..5).map(|i| keychain(7.into(), PeerId::from(i as u8))),
        )
        .expect("passed nodes set is non-empty");
        match multisigned {
            PartiallyMultisigned::Incomplete { unchecked } => assert_eq!(
                handler.on_multisigned_hash(unchecked),
                Err(Error::BadMultisignature)
            ),
            PartiallyMultisigned::Complete { .. } => {
                panic!("multisignature should not be complete")
            }
        }
    }

    use aleph_bft_crypto::PeerId;
    #[allow(unused)]
    fn _suppress() {
        let _ = NumPeers::from(7);
    }
}
