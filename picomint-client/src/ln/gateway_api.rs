//! Client-side iroh dispatcher for the gateway's public API. Mirrors
//! `picomint_client::api::request_over_connection` — one bi-stream per call
//! on a live `Connection`, consensus-encoded [`GatewayMethod`] out,
//! consensus-encoded `Result<Vec<u8>, ApiError>` back, decoded into the
//! per-method response newtype.

use bitcoin::hashes::sha256;
use bitcoin::secp256k1::schnorr::Signature;
use iroh::endpoint::Connection;
use lightning_invoice::Bolt11Invoice;
use picomint_core::config::FederationId;
use picomint_core::ln::contracts::{IncomingContract, OutgoingContract};
use picomint_core::ln::gateway_api::{
    CreateBolt11InvoiceRequest, CreateBolt11InvoiceResponse, GATEWAY_MAX_MESSAGE_BYTES,
    GatewayInfo, GatewayInfoRequest, GatewayInfoResponse, GatewayMethod, SendPaymentRequest,
    SendPaymentResponse, VerifyBolt11PreimageRequest, VerifyBolt11PreimageResponse,
};
use picomint_core::ln::{Bolt11InvoiceDescription, LightningInvoice};
use picomint_core::module::ApiError;
use picomint_core::{Amount, OutPoint};
use picomint_encoding::{Decodable, Encodable};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("Could not open bi-stream to gateway: {0}")]
    OpenBi(String),

    #[error("Write to gateway failed: {0}")]
    Write(String),

    #[error("Read from gateway failed: {0}")]
    Read(String),

    #[error("Failed to decode gateway response")]
    InvalidResponse,

    #[error("Gateway returned an error: {0}")]
    ApiError(ApiError),
}

async fn request<Resp: Decodable>(
    method: GatewayMethod,
    connection: &Connection,
) -> Result<Resp, GatewayError> {
    let request_bytes = method.consensus_encode_to_vec();

    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|e| GatewayError::OpenBi(e.to_string()))?;

    send.write_all(&request_bytes)
        .await
        .map_err(|e| GatewayError::Write(e.to_string()))?;
    send.finish()
        .map_err(|e| GatewayError::Write(e.to_string()))?;

    let response_bytes = recv
        .read_to_end(GATEWAY_MAX_MESSAGE_BYTES)
        .await
        .map_err(|e| GatewayError::Read(e.to_string()))?;

    let payload = <Result<Vec<u8>, ApiError>>::consensus_decode_exact(&response_bytes)
        .map_err(|_| GatewayError::InvalidResponse)?
        .map_err(GatewayError::ApiError)?;

    Resp::consensus_decode_exact(&payload).map_err(|_| GatewayError::InvalidResponse)
}

pub async fn gateway_info(
    connection: &Connection,
    federation_id: &FederationId,
) -> Result<GatewayInfo, GatewayError> {
    let resp: GatewayInfoResponse = request(
        GatewayMethod::GatewayInfo(GatewayInfoRequest {
            federation_id: *federation_id,
        }),
        connection,
    )
    .await?;
    Ok(resp.gateway_info)
}

pub async fn bolt11_invoice(
    connection: &Connection,
    federation_id: FederationId,
    contract: IncomingContract,
    amount: Amount,
    description: Bolt11InvoiceDescription,
    expiry_secs: u32,
) -> Result<Bolt11Invoice, GatewayError> {
    let resp: CreateBolt11InvoiceResponse = request(
        GatewayMethod::CreateBolt11Invoice(CreateBolt11InvoiceRequest {
            federation_id,
            contract,
            amount,
            description,
            expiry_secs,
        }),
        connection,
    )
    .await?;
    Ok(resp.invoice)
}

pub async fn send_payment(
    connection: &Connection,
    federation_id: FederationId,
    outpoint: OutPoint,
    contract: OutgoingContract,
    invoice: LightningInvoice,
    auth: Signature,
) -> Result<Result<[u8; 32], Signature>, GatewayError> {
    let resp: SendPaymentResponse = request(
        GatewayMethod::SendPayment(SendPaymentRequest {
            federation_id,
            outpoint,
            contract,
            invoice,
            auth,
        }),
        connection,
    )
    .await?;
    Ok(resp.outcome)
}

#[allow(dead_code)]
pub async fn verify_bolt11_preimage(
    connection: &Connection,
    payment_hash: sha256::Hash,
    wait: bool,
) -> Result<VerifyBolt11PreimageResponse, GatewayError> {
    request(
        GatewayMethod::VerifyBolt11Preimage(VerifyBolt11PreimageRequest { payment_hash, wait }),
        connection,
    )
    .await
}
