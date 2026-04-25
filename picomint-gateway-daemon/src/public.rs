//! Public client-facing API served over iroh. Dispatch is structured
//! the same way as the federation server's: `mod rpc` holds typed
//! `fn(state, XRequest) -> Result<XResponse, ApiError>` handlers, and
//! [`dispatch`] routes [`GatewayMethod`] variants into them via the
//! `handler!`/`handler_async!` macros from `picomint-iroh-api`.

use iroh::Endpoint;
use picomint_core::ln::gateway_api::GatewayMethod;
use picomint_core::module::ApiError;
use picomint_core::task::{TaskGroup, TaskHandle};
use picomint_iroh_api::handler_async;

use crate::AppState;

mod rpc {
    use lightning_invoice::Bolt11Invoice;
    use picomint_core::ln::gateway_api::{
        CreateBolt11InvoiceRequest, CreateBolt11InvoiceResponse, GatewayInfo, GatewayInfoRequest,
        GatewayInfoResponse, SendPaymentRequest, SendPaymentResponse, VerifyBolt11PreimageRequest,
        VerifyBolt11PreimageResponse,
    };
    use picomint_core::module::ApiError;
    use picomint_lnurl::VerifyResponse;

    use crate::AppState;

    pub async fn gateway_info(
        state: &AppState,
        req: GatewayInfoRequest,
    ) -> Result<GatewayInfoResponse, ApiError> {
        let gateway_info: GatewayInfo = state
            .gateway_info(&req.federation_id)
            .await
            .map_err(|e| ApiError::bad_request(e.to_string()))?
            .ok_or_else(|| {
                ApiError::bad_request(format!("Federation {} is not connected", req.federation_id))
            })?;

        Ok(GatewayInfoResponse { gateway_info })
    }

    pub async fn send_payment(
        state: &AppState,
        req: SendPaymentRequest,
    ) -> Result<SendPaymentResponse, ApiError> {
        let outcome = state
            .send_payment(req)
            .await
            .map_err(|e| ApiError::bad_request(e.to_string()))?;

        Ok(SendPaymentResponse { outcome })
    }

    pub async fn create_bolt11_invoice(
        state: &AppState,
        req: CreateBolt11InvoiceRequest,
    ) -> Result<CreateBolt11InvoiceResponse, ApiError> {
        let invoice: Bolt11Invoice = state
            .create_bolt11_invoice(req)
            .await
            .map_err(|e| ApiError::bad_request(e.to_string()))?;

        Ok(CreateBolt11InvoiceResponse { invoice })
    }

    pub async fn verify_bolt11_preimage(
        state: &AppState,
        req: VerifyBolt11PreimageRequest,
    ) -> Result<VerifyBolt11PreimageResponse, ApiError> {
        let VerifyResponse { settled, preimage } = state
            .verify_bolt11_preimage(req.payment_hash, req.wait)
            .await
            .map_err(ApiError::bad_request)?;

        Ok(VerifyBolt11PreimageResponse { settled, preimage })
    }
}

pub async fn run_public(
    state: AppState,
    endpoint: Endpoint,
    task_group: TaskGroup,
    handle: TaskHandle,
) {
    let (foreign_conn_tx, foreign_conn_rx) = async_channel::bounded(128);

    task_group.spawn_cancellable("public-accept", {
        let endpoint = endpoint.clone();
        async move {
            tokio::select! {
                () = picomint_iroh_api::accept_into_channel(endpoint, foreign_conn_tx) => {},
                () = handle.make_shutdown_rx() => {},
            }
        }
    });

    picomint_iroh_api::run_iroh_api(
        foreign_conn_rx,
        move |method: GatewayMethod| {
            let state = state.clone();
            async move { dispatch(state, method).await }
        },
        task_group,
    )
    .await;
}

async fn dispatch(state: AppState, method: GatewayMethod) -> Result<Vec<u8>, ApiError> {
    match method {
        GatewayMethod::GatewayInfo(req) => handler_async!(gateway_info, &state, req).await,
        GatewayMethod::SendPayment(req) => handler_async!(send_payment, &state, req).await,
        GatewayMethod::CreateBolt11Invoice(req) => {
            handler_async!(create_bolt11_invoice, &state, req).await
        }
        GatewayMethod::VerifyBolt11Preimage(req) => {
            handler_async!(verify_bolt11_preimage, &state, req).await
        }
    }
}
