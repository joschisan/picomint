//! Generic iroh request/response API loop. One bi-stream per request,
//! consensus-encoded `Method` in, consensus-encoded `Result<Vec<u8>,
//! ApiError>` back.
//!
//! Shared by the federation server's public API
//! (`picomint_server_daemon::consensus`), the gateway daemon's public
//! API (`picomint_gateway_daemon::public`), and the integration-test
//! mock gateway (`picomint_integration_tests::ln`).

use std::sync::Arc;

use futures::FutureExt;
use iroh::Endpoint;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use picomint_core::module::ApiError;
use picomint_core::task::TaskGroup;
use picomint_encoding::{Decodable, Encodable};
use picomint_logging::LOG_NET_API;
use tokio::sync::Semaphore;
use tracing::warn;

/// Maximum number of concurrent iroh connections on the public API.
const MAX_CONNECTIONS: usize = 1000;

/// Maximum number of parallel requests per iroh API connection.
const MAX_REQUESTS_PER_CONNECTION: usize = 50;

/// Maximum encoded request size accepted on a single bi-stream.
const MAX_REQUEST_BYTES: usize = 100_000;

/// Drive the request/response API loop. Reads connections from
/// `foreign_conn_rx`, decodes each request as `M`, calls `handler` for
/// dispatch, and writes the consensus-encoded response back.
///
/// Callers responsible for filling the channel: the federation server's
/// p2p demux feeds it from one side; gateway/mock callers can use
/// [`accept_into_channel`] to plumb an [`Endpoint`] into it.
pub async fn run_iroh_api<M, F, Fut>(
    foreign_conn_rx: async_channel::Receiver<Connection>,
    handler: F,
    task_group: TaskGroup,
) where
    M: Decodable + Send + 'static,
    F: Fn(M) -> Fut + Clone + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Vec<u8>, ApiError>> + Send + 'static,
{
    let parallel_connections_limit = Arc::new(Semaphore::new(MAX_CONNECTIONS));

    while let Ok(connection) = foreign_conn_rx.recv().await {
        if parallel_connections_limit.available_permits() == 0 {
            warn!(
                target: LOG_NET_API,
                limit = MAX_CONNECTIONS,
                "Iroh API connection limit reached, blocking new connections"
            );
        }
        let permit = parallel_connections_limit
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore should not be closed");
        let handler = handler.clone();
        let task_group_inner = task_group.clone();
        task_group.spawn_cancellable_silent(
            "handle-iroh-connection",
            handle_incoming(handler, task_group_inner, connection, permit).then(|result| async {
                if let Err(err) = result {
                    warn!(target: LOG_NET_API, err = %format_args!("{err:#}"), "Failed to handle iroh connection");
                }
            }),
        );
    }
}

async fn handle_incoming<M, F, Fut>(
    handler: F,
    task_group: TaskGroup,
    connection: Connection,
    _connection_permit: tokio::sync::OwnedSemaphorePermit,
) -> anyhow::Result<()>
where
    M: Decodable + Send + 'static,
    F: Fn(M) -> Fut + Clone + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Vec<u8>, ApiError>> + Send + 'static,
{
    let parallel_requests_limit = Arc::new(Semaphore::new(MAX_REQUESTS_PER_CONNECTION));

    loop {
        let (send_stream, recv_stream) = connection.accept_bi().await?;

        if parallel_requests_limit.available_permits() == 0 {
            warn!(
                target: LOG_NET_API,
                limit = MAX_REQUESTS_PER_CONNECTION,
                "Iroh API request limit reached for connection, blocking new requests"
            );
        }
        let permit = parallel_requests_limit
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore should not be closed");
        let handler = handler.clone();
        task_group.spawn_cancellable_silent(
            "handle-iroh-request",
            handle_request(handler, send_stream, recv_stream, permit).then(|result| async {
                if let Err(err) = result {
                    warn!(target: LOG_NET_API, err = %format_args!("{err:#}"), "Failed to handle iroh request");
                }
            }),
        );
    }
}

async fn handle_request<M, F, Fut>(
    handler: F,
    mut send_stream: SendStream,
    mut recv_stream: RecvStream,
    _request_permit: tokio::sync::OwnedSemaphorePermit,
) -> anyhow::Result<()>
where
    M: Decodable,
    F: Fn(M) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, ApiError>> + Send,
{
    let request = recv_stream.read_to_end(MAX_REQUEST_BYTES).await?;
    let method = M::consensus_decode_exact(&request)?;
    let response = handler(method).await;
    let response = response.consensus_encode_to_vec();
    send_stream.write_all(&response).await?;
    send_stream.finish()?;
    Ok(())
}

/// Plumb [`Endpoint::accept`] into a channel suitable for
/// [`run_iroh_api`]. Performs the iroh handshake and pushes
/// fully-established connections; failures are logged and dropped.
///
/// Exits when the endpoint stops yielding incoming connections or the
/// channel's receiver is dropped.
pub async fn accept_into_channel(
    endpoint: Endpoint,
    foreign_conn_tx: async_channel::Sender<Connection>,
) {
    while let Some(incoming) = endpoint.accept().await {
        let connecting = match incoming.accept() {
            Ok(c) => c,
            Err(e) => {
                warn!(target: LOG_NET_API, err = %e, "Iroh accept failed");
                continue;
            }
        };
        let connection = match connecting.await {
            Ok(c) => c,
            Err(e) => {
                warn!(target: LOG_NET_API, err = %e, "Iroh handshake failed");
                continue;
            }
        };
        if foreign_conn_tx.send(connection).await.is_err() {
            break;
        }
    }
}

/// Dispatch helper for module `handle_api` match arms.
///
/// `handler!(fn_name, self, req).await` calls `rpc::fn_name(self, req)`
/// and consensus-encodes the typed response. Each module has a `mod
/// rpc` submodule with one `fn name(module: &Self, req: XRequest) ->
/// Result<XResponse, ApiError>` per endpoint. Use [`handler_async!`]
/// when the rpc handler is itself async.
#[macro_export]
macro_rules! handler {
    ($func:ident, $self:expr, $req:expr) => {
        async move {
            let resp = rpc::$func($self, $req)?;
            ::std::result::Result::Ok(::picomint_encoding::Encodable::consensus_encode_to_vec(
                &resp,
            ))
        }
    };
}

/// Like [`handler!`] but for `async fn` rpc handlers.
#[macro_export]
macro_rules! handler_async {
    ($func:ident, $self:expr, $req:expr) => {
        async move {
            let resp = rpc::$func($self, $req).await?;
            ::std::result::Result::Ok(::picomint_encoding::Encodable::consensus_encode_to_vec(
                &resp,
            ))
        }
    };
}
