//! Client-side iroh RPC calls to the gateway daemon. The wire types
//! ([`GatewayMethod`] + per-method `*Request`/`*Response` structs) live in
//! [`picomint_core::ln::methods`] because the gateway daemon must
//! agree on them. The wire envelope is `Result<Vec<u8>, String>` — same
//! shape as the federation API: bytes are the response struct
//! consensus-encoded.

use bitcoin::secp256k1::schnorr::Signature;
use iroh::Endpoint;
use lightning_invoice::Bolt11Invoice;
use picomint_core::OutPoint;
use picomint_core::config::FederationId;
use picomint_core::ln::LightningInvoice;
use picomint_core::ln::contracts::{IncomingContract, OutgoingContract};
use picomint_core::ln::gateway::{GatewayInfo, GatewayPk};
use picomint_core::ln::methods::{
    GatewayMethod, InfoRequest, InfoResponse, ReceiveRequest, ReceiveResponse, SendRequest,
    SendResponse,
};

pub async fn gateway_info(
    endpoint: &Endpoint,
    gateway_pk: GatewayPk,
    federation: FederationId,
) -> anyhow::Result<Option<GatewayInfo>> {
    picomint_rpc::request::<_, InfoResponse>(
        endpoint,
        gateway_pk.0,
        GatewayMethod::Info(InfoRequest { federation }),
    )
    .await
    .map(|r| r.info)
}

pub async fn receive(
    endpoint: &Endpoint,
    gateway_pk: GatewayPk,
    federation: FederationId,
    contract: IncomingContract,
) -> anyhow::Result<Bolt11Invoice> {
    picomint_rpc::request::<_, ReceiveResponse>(
        endpoint,
        gateway_pk.0,
        GatewayMethod::Receive(ReceiveRequest {
            federation,
            contract,
        }),
    )
    .await
    .map(|r| r.invoice)
}

pub async fn send(
    endpoint: &Endpoint,
    gateway_pk: GatewayPk,
    federation: FederationId,
    outpoint: OutPoint,
    contract: OutgoingContract,
    invoice: LightningInvoice,
    auth: Signature,
) -> anyhow::Result<Result<[u8; 32], Signature>> {
    picomint_rpc::request::<_, SendResponse>(
        endpoint,
        gateway_pk.0,
        GatewayMethod::Send(SendRequest {
            federation,
            outpoint,
            contract,
            invoice,
            auth,
        }),
    )
    .await
    .map(|r| r.result)
}
