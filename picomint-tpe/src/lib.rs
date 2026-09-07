//! # Threshold Point Encryption
//!
//! Every value is held as its bytes and decoded at most once, on first use —
//! see `picomint_encoding::lazy_bytes`. A value built from a curve element
//! always decodes; one decoded from the wire or storage may not, which is
//! why the operations that need the element are fallible.

use std::collections::BTreeMap;
use std::ops::Mul;

use bitcoin_hashes::{Hash, sha256};
use bls12_381::{G1Affine, G1Projective, G2Affine, G2Projective, Scalar, pairing};
use group::ff::Field;
use group::{Curve, Group};
use picomint_encoding::{Decodable, Encodable, bls_g1, bls_g2, bls_scalar};
use rand_chacha::ChaChaRng;
use rand_chacha::rand_core::SeedableRng;
use serde::{Deserialize, Serialize};

const TAG: [u8; 30] = *b"PICOMINT_TPE_BLS12_381_MESSAGE";

bls_scalar!(SecretKeyShare);
bls_g1!(PublicKeyShare);
bls_g1!(AggregatePublicKey);
bls_g1!(DecryptionKeyShare);
bls_g1!(AggregateDecryptionKey);
bls_g1!(EphemeralPublicKey);
bls_g2!(EphemeralSignature);

#[derive(Clone, Debug, Eq, PartialEq, Hash, Encodable, Decodable, Serialize, Deserialize)]
pub struct CipherText {
    pub encrypted_preimage: [u8; 32],
    pub pk: EphemeralPublicKey,
    pub signature: EphemeralSignature,
}

pub fn derive_pk_share(sk: &SecretKeyShare) -> Option<PublicKeyShare> {
    sk.scalar()
        .map(|sk| G1Projective::generator().mul(sk).to_affine().into())
}

pub fn encrypt_preimage(
    agg_pk: &AggregatePublicKey,
    encryption_seed: &[u8; 32],
    preimage: &[u8; 32],
    commitment: &sha256::Hash,
) -> Option<CipherText> {
    let agg_dk = derive_agg_dk(agg_pk, encryption_seed)?;
    let encrypted_preimage = xor_with_hash(*preimage, &agg_dk);

    let ephemeral_sk = derive_ephemeral_sk(encryption_seed);
    let ephemeral_pk = G1Projective::generator().mul(ephemeral_sk).to_affine();
    let ephemeral_signature = hash_to_message(&encrypted_preimage, &ephemeral_pk, commitment)
        .mul(ephemeral_sk)
        .to_affine();

    Some(CipherText {
        encrypted_preimage,
        pk: ephemeral_pk.into(),
        signature: ephemeral_signature.into(),
    })
}

pub fn derive_agg_dk(
    agg_pk: &AggregatePublicKey,
    encryption_seed: &[u8; 32],
) -> Option<AggregateDecryptionKey> {
    agg_pk.point().map(|pk| {
        pk.mul(derive_ephemeral_sk(encryption_seed))
            .to_affine()
            .into()
    })
}

fn derive_ephemeral_sk(encryption_seed: &[u8; 32]) -> Scalar {
    Scalar::random(&mut ChaChaRng::from_seed(*encryption_seed))
}

fn xor_with_hash(mut bytes: [u8; 32], agg_dk: &AggregateDecryptionKey) -> [u8; 32] {
    let hash = sha256::Hash::hash(agg_dk.as_bytes());

    for i in 0..32 {
        bytes[i] ^= hash[i];
    }

    bytes
}

fn hash_to_message(
    encrypted_point: &[u8; 32],
    ephemeral_pk: &G1Affine,
    commitment: &sha256::Hash,
) -> G2Affine {
    let seed = (
        TAG,
        encrypted_point,
        ephemeral_pk.to_compressed(),
        commitment,
    )
        .consensus_hash::<sha256::Hash>()
        .to_byte_array();

    G2Projective::random(&mut ChaChaRng::from_seed(seed)).to_affine()
}

pub fn verify_ciphertext(ct: &CipherText, commitment: &sha256::Hash) -> bool {
    let (Some(pk), Some(signature)) = (ct.pk.point(), ct.signature.point()) else {
        return false;
    };

    let message = hash_to_message(&ct.encrypted_preimage, pk, commitment);

    pairing(&G1Affine::generator(), signature) == pairing(pk, &message)
}

pub fn decrypt_preimage(ct: &CipherText, agg_dk: &AggregateDecryptionKey) -> [u8; 32] {
    xor_with_hash(ct.encrypted_preimage, agg_dk)
}

pub fn verify_agg_dk(
    agg_pk: &AggregatePublicKey,
    agg_dk: &AggregateDecryptionKey,
    ct: &CipherText,
    commitment: &sha256::Hash,
) -> bool {
    let (Some(pk), Some(signature), Some(agg_pk), Some(agg_dk)) = (
        ct.pk.point(),
        ct.signature.point(),
        agg_pk.point(),
        agg_dk.point(),
    ) else {
        return false;
    };

    let message = hash_to_message(&ct.encrypted_preimage, pk, commitment);

    assert_eq!(
        pairing(&G1Affine::generator(), signature),
        pairing(pk, &message)
    );

    pairing(agg_dk, &message) == pairing(agg_pk, signature)
}

/// `None` if the ciphertext's ephemeral key is not a point of the
/// prime-order subgroup — a small-order point would leak the key share.
pub fn create_dk_share(sks: &SecretKeyShare, ct: &CipherText) -> Option<DecryptionKeyShare> {
    ct.pk
        .point()
        .zip(sks.scalar())
        .map(|(pk, sk)| pk.mul(sk).to_affine().into())
}

pub fn verify_dk_share(
    pks: &PublicKeyShare,
    dks: &DecryptionKeyShare,
    ct: &CipherText,
    commitment: &sha256::Hash,
) -> bool {
    let (Some(pk), Some(signature), Some(pks), Some(dks)) = (
        ct.pk.point(),
        ct.signature.point(),
        pks.point(),
        dks.point(),
    ) else {
        return false;
    };

    let message = hash_to_message(&ct.encrypted_preimage, pk, commitment);

    assert_eq!(
        pairing(&G1Affine::generator(), signature),
        pairing(pk, &message)
    );

    pairing(dks, &message) == pairing(pks, signature)
}

/// `None` if a share is not a point.
pub fn aggregate_dk_shares(
    shares: &BTreeMap<u64, DecryptionKeyShare>,
) -> Option<AggregateDecryptionKey> {
    let points = shares
        .values()
        .map(|share| share.point().copied())
        .collect::<Option<Vec<_>>>()?;

    let agg_dk = lagrange_multipliers(
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

    Some(agg_dk.to_affine().into())
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

#[cfg(test)]
mod tests;
