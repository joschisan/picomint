use std::collections::BTreeMap;

use bitcoin::hashes::Hash as BitcoinHash;
use bitcoin::hashes::sha256;
use bls12_381::{G2Projective, Scalar};
use group::Curve;
use group::ff::Field;
use rand::SeedableRng;
use rand::rngs::OsRng;
use rand_chacha::ChaChaRng;

use crate::{
    AggregatePublicKey, BlindedSignatureShare, BlindingKey, Nonce, PublicKeyShare, SecretKeyShare,
    aggregate_signature_shares, blind_nonce, derive_pk_share, sign_nonce, unblind_signature,
    verify, verify_signature_share,
};

fn dealer_agg_pk() -> AggregatePublicKey {
    (G2Projective::generator() * coefficient(0))
        .to_affine()
        .into()
}

fn dealer_pk(threshold: u64, node: u64) -> PublicKeyShare {
    derive_pk_share(&dealer_sk(threshold, node)).unwrap()
}

fn dealer_sk(threshold: u64, node: u64) -> SecretKeyShare {
    let x = Scalar::from(node + 1);

    // We evaluate the scalar polynomial of degree threshold - 1 at the point x
    // using the Horner schema.

    let y = (0..threshold)
        .map(coefficient)
        .rev()
        .reduce(|accumulator, c| accumulator * x + c)
        .expect("We have at least one coefficient");

    y.into()
}

fn coefficient(index: u64) -> Scalar {
    Scalar::random(&mut ChaChaRng::from_seed(
        *sha256::Hash::hash(&index.to_be_bytes()).as_byte_array(),
    ))
}

#[test]
fn test_roundtrip() {
    const NODES: u64 = 4;
    const THRESHOLD: u64 = 3;

    let nonce = Nonce::from_public_key([7_u8; 32]);
    let blinding_key = BlindingKey::from(Scalar::random(OsRng));

    let b_message = blind_nonce(&nonce, &blinding_key).unwrap();

    for node in 0..NODES {
        assert!(verify_signature_share(
            &b_message,
            &sign_nonce(&b_message, &dealer_sk(THRESHOLD, node)).unwrap(),
            &dealer_pk(THRESHOLD, node)
        ));
    }

    let signature_shares = (0..THRESHOLD)
        .map(|node| {
            (
                node,
                sign_nonce(&b_message, &dealer_sk(THRESHOLD, node)).unwrap(),
            )
        })
        .collect::<BTreeMap<u64, BlindedSignatureShare>>();

    let signature = aggregate_signature_shares(&signature_shares).unwrap();

    let signature = unblind_signature(&blinding_key, &signature).unwrap();

    assert!(verify(&nonce, &signature, &dealer_agg_pk()));
}
