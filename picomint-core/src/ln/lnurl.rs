use bitcoin::secp256k1;
use picomint_encoding::{Decodable, Encodable};

use crate::config::FederationId;
use crate::ln::gateway_api::GatewayPk;
use serde::{Deserialize, Serialize};
use tpe::AggregatePublicKey;

#[derive(Debug, Clone, Serialize, Deserialize, Encodable, Decodable)]
pub struct LnurlRequest {
    pub federation_id: FederationId,
    pub recipient_pk: secp256k1::PublicKey,
    pub aggregate_pk: AggregatePublicKey,
    pub gateways: Vec<GatewayPk>,
}
