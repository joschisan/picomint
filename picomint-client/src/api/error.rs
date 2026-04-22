use std::collections::BTreeMap;

use picomint_core::PeerId;
use picomint_core::module::ApiMethod;

use super::ServerError;

/// An API request error when calling an entire federation.
///
/// Generally all Federation errors are retryable.
#[derive(Debug)]
pub struct FederationError {
    pub method: ApiMethod,
    pub peer_errors: BTreeMap<PeerId, ServerError>,
}

impl FederationError {
    pub(crate) fn peer_errors(
        method: ApiMethod,
        peer_errors: BTreeMap<PeerId, ServerError>,
    ) -> Self {
        Self {
            method,
            peer_errors,
        }
    }

    pub fn new_one_peer(peer_id: PeerId, method: ApiMethod, error: ServerError) -> Self {
        Self {
            method,
            peer_errors: [(peer_id, error)].into_iter().collect(),
        }
    }
}
