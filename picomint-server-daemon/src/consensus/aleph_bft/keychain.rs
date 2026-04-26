//! Build a [`Schnorr`] keychain from the server's persisted [`ServerConfig`].
//!
//! The actual schnorr-backed implementation lives in
//! `picomint-aleph-bft-crypto` so it can be reused by the consensus internals
//! during the trait → concrete-type migration. This module is just the
//! glue that projects the relevant fields out of the daemon's config.

pub use aleph_bft::Schnorr;
use std::collections::BTreeMap;

use picomint_core::PeerId;
use picomint_core::secp256k1::PublicKey;

use crate::config::ServerConfig;

/// Construct a [`Schnorr`] keychain from `cfg` — this peer's identity, this
/// peer's broadcast secret key, and the federation's broadcast public-key set.
pub fn from_cfg(cfg: &ServerConfig) -> Schnorr {
    let public_keys: BTreeMap<PeerId, PublicKey> = cfg
        .consensus
        .peers
        .iter()
        .map(|(id, endpoint)| (*id, endpoint.broadcast_pk))
        .collect();
    Schnorr::new(public_keys, cfg.private.identity, cfg.private.broadcast_secret_key)
}
