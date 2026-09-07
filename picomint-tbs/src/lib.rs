//! # Threshold Blind Signatures
//!
//! This library implements an ad-hoc threshold blind signature scheme based on
//! BLS signatures using the (unrelated) BLS12-381 curve.
//!
//! Every value is held as its bytes and decoded at most once, on first use —
//! see `picomint_encoding::lazy_bytes`. A value built from a curve element
//! always decodes; one decoded from the wire or storage may not, which is
//! why the operations that need the element are fallible.

use std::collections::BTreeMap;

use bitcoin::hashes::Hash as BitcoinHash;
use bitcoin::hashes::sha256;
use bls12_381::{G1Projective, G2Affine, G2Projective, Scalar, pairing};
use group::{Curve, Group};
use picomint_encoding::{Encodable, bls_g1, bls_g2, bls_scalar};
use rand::SeedableRng;
use rand_chacha::ChaChaRng;

const TAG: [u8; 28] = *b"PICOMINT_TBS_BLS12_381_NONCE";

bls_scalar!(SecretKeyShare);
bls_g2!(PublicKeyShare);
bls_g2!(AggregatePublicKey);
bls_g1!(Nonce);
bls_scalar!(BlindingKey);
bls_g1!(BlindedNonce);
bls_g1!(BlindedSignatureShare);
bls_g1!(BlindedSignature);
bls_g1!(Signature);

pub fn derive_pk_share(sk: &SecretKeyShare) -> Option<PublicKeyShare> {
    sk.scalar()
        .map(|sk| (G2Projective::generator() * sk).to_affine().into())
}

impl Nonce {
    /// Creates a [`Nonce`] by hashing a 32-byte x-only public key with
    /// SHA-256 under the domain separator `PICOMINT_TBS_BLS12_381_NONCE`,
    /// then mapping the hash to a BLS12-381 G1 curve point via a seeded
    /// [`ChaChaRng`].
    pub fn from_public_key(bytes: [u8; 32]) -> Nonce {
        let seed = (TAG, bytes)
            .consensus_hash::<sha256::Hash>()
            .to_byte_array();

        G1Projective::random(&mut ChaChaRng::from_seed(seed))
            .to_affine()
            .into()
    }
}

pub fn blind_nonce(nonce: &Nonce, blinding_key: &BlindingKey) -> Option<BlindedNonce> {
    nonce
        .point()
        .zip(blinding_key.scalar())
        .map(|(nonce, key)| (nonce * key).to_affine().into())
}

/// `None` if the nonce is not a point of the prime-order subgroup — a
/// small-order point would leak the key share.
pub fn sign_nonce(nonce: &BlindedNonce, sks: &SecretKeyShare) -> Option<BlindedSignatureShare> {
    nonce
        .point()
        .zip(sks.scalar())
        .map(|(nonce, sk)| (nonce * sk).to_affine().into())
}

pub fn verify_signature_share(
    nonce: &BlindedNonce,
    sig: &BlindedSignatureShare,
    pk: &PublicKeyShare,
) -> bool {
    let (Some(nonce), Some(sig), Some(pk)) = (nonce.point(), sig.point(), pk.point()) else {
        return false;
    };

    pairing(nonce, pk) == pairing(sig, &G2Affine::generator())
}

/// Combines the exact threshold of valid blinded signature shares to a blinded
/// signature, `None` if a share is not a point. The responsibility of
/// verifying the shares and supplying exactly the necessary threshold of
/// shares lies with the caller.
/// # Panics
/// If shares is empty
pub fn aggregate_signature_shares(
    shares: &BTreeMap<u64, BlindedSignatureShare>,
) -> Option<BlindedSignature> {
    let points = shares
        .values()
        .map(|share| share.point().copied())
        .collect::<Option<Vec<_>>>()?;

    // this is a special case for one-of-one mints
    if let [point] = points[..] {
        return Some(point.into());
    }

    let sig = lagrange_multipliers(
        shares
            .keys()
            .cloned()
            .map(|node| Scalar::from(node + 1))
            .collect(),
    )
    .into_iter()
    .zip(points)
    .map(|(lagrange_multiplier, point)| lagrange_multiplier * point)
    .reduce(|a, b| a + b)
    .expect("We have at least one share");

    Some(sig.to_affine().into())
}

fn lagrange_multipliers(scalars: Vec<Scalar>) -> Vec<Scalar> {
    scalars
        .iter()
        .map(|i| {
            scalars
                .iter()
                .filter(|j| *j != i)
                .map(|j| j * (j - i).invert().expect("We filtered the case j == i"))
                .reduce(|a, b| a * b)
                .expect("We have at least one share")
        })
        .collect()
}

pub fn verify_blinded_signature(
    nonce: &BlindedNonce,
    sig: &BlindedSignature,
    pk: &AggregatePublicKey,
) -> bool {
    let (Some(nonce), Some(sig), Some(pk)) = (nonce.point(), sig.point(), pk.point()) else {
        return false;
    };

    pairing(nonce, pk) == pairing(sig, &G2Affine::generator())
}

pub fn unblind_signature(
    blinding_key: &BlindingKey,
    blinded_sig: &BlindedSignature,
) -> Option<Signature> {
    let inverse = blinding_key
        .scalar()?
        .invert()
        .expect("a blinding key is a non-zero scalar");

    blinded_sig
        .point()
        .map(|sig| (sig * inverse).to_affine().into())
}

pub fn verify(nonce: &Nonce, sig: &Signature, pk: &AggregatePublicKey) -> bool {
    let (Some(nonce), Some(sig), Some(pk)) = (nonce.point(), sig.point(), pk.point()) else {
        return false;
    };

    pairing(nonce, pk) == pairing(sig, &G2Affine::generator())
}

#[cfg(test)]
mod tests;
