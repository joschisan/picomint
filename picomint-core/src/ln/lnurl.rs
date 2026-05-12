use bitcoin::secp256k1;
use picomint_encoding::{Decodable, Encodable};

use crate::config::FederationId;
use crate::ln::gateway_api::GatewayPk;
use serde::{Deserialize, Serialize};
use tpe::AggregatePublicKey;

/// Maximum number of gateways embedded in a single `LnurlRequest`. The
/// LNURL daemon probes them in parallel, so capping keeps the fan-out
/// bounded and the encoded payload small.
pub const MAX_GATEWAYS_PER_LNURL: usize = 5;

#[derive(Debug, Clone, Serialize, Deserialize, Encodable, Decodable)]
pub struct LnurlRequest {
    pub federation_id: FederationId,
    pub recipient_pk: secp256k1::PublicKey,
    pub aggregate_pk: AggregatePublicKey,
    pub gateways: Vec<GatewayPk>,
}
