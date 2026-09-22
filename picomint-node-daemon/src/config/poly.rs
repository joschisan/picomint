//! BLS polynomial helpers used by the G2 DKG.

use bls12_381::{G2Affine, G2Projective, Scalar};
use group::Curve;
use picomint_core::NodeId;

pub fn g2(scalar: &Scalar) -> G2Projective {
    G2Projective::generator() * scalar
}

// Offset by 1, since evaluating a poly at 0 reveals the secret
pub fn scalar(node: &NodeId) -> Scalar {
    Scalar::from(node.to_usize() as u64 + 1)
}

pub fn eval_poly_g2(coefficients: &[G2Projective], node: &NodeId) -> G2Affine {
    coefficients
        .iter()
        .copied()
        .rev()
        .reduce(|acc, coefficient| acc * scalar(node) + coefficient)
        .expect("We have at least one coefficient")
        .to_affine()
}
