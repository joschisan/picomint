//! Client-side iroh RPC calls to the gateway daemon. The wire types
//! ([`GatewayMethod`] + per-method `*Request`/`*Response` structs) live in
//! [`picomint_core::ln::gateway_api`] because the gateway daemon must
//! agree on them. The wire envelope is `Result<Vec<u8>, String>` — same
//! shape as the federation API: bytes are the response struct
//! consensus-encoded.

use bitcoin::secp256k1::schnorr::Signature;
use iroh::Endpoint;
use lightning_invoice::Bolt11Invoice;
use picomint_core::config::FederationId;
use picomint_core::ln::LightningInvoice;
use picomint_core::ln::contracts::{IncomingContract, OutgoingContract};
use picomint_core::ln::gateway_api::{
    CreateInvoiceRequest, CreateInvoiceResponse, GatewayInfo, GatewayMethod, GatewayPk,
    InfoRequest, InfoResponse, SendPaymentRequest, SendPaymentResponse,
};
use picomint_core::{Amount, OutPoint};

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

pub async fn create_bolt11_invoice(
    endpoint: &Endpoint,
    gateway_pk: GatewayPk,
    federation: FederationId,
    contract: IncomingContract,
    amount: Amount,
    expiry_secs: u32,
) -> anyhow::Result<Bolt11Invoice> {
    picomint_rpc::request::<_, CreateInvoiceResponse>(
        endpoint,
        gateway_pk.0,
        GatewayMethod::CreateInvoice(CreateInvoiceRequest {
            federation,
            contract,
            amount,
            expiry_secs,
        }),
    )
    .await
    .map(|r| r.invoice)
}

pub async fn send_payment(
    endpoint: &Endpoint,
    gateway_pk: GatewayPk,
    federation: FederationId,
    outpoint: OutPoint,
    contract: OutgoingContract,
    invoice: LightningInvoice,
    auth: Signature,
) -> anyhow::Result<Result<[u8; 32], Signature>> {
    picomint_rpc::request::<_, SendPaymentResponse>(
        endpoint,
        gateway_pk.0,
        GatewayMethod::SendPayment(SendPaymentRequest {
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
