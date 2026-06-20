//! iroh RPC primitives shared by picomint client and server.
//!
//! One request = one bidirectional stream. Connections are kept alive and
//! reused: the federation client holds a pooled connection per peer (see
//! `picomint-client`'s `FederationApi`) and multiplexes every request as a
//! fresh bi stream over it via [`request_on_connection`], paying the QUIC
//! handshake and hole-punched path once rather than per request. [`request`]
//! remains a one-shot convenience (connect → one request → close) for
//! callers without a pool, e.g. fetching the config from an invite code.
//!
//! Server-side, [`handle_request`] serves a connection by accepting bi
//! streams in a loop until the peer closes, handling each as one request
//! (accept_bi → decode → handler → encode → finish) on its own task.
//!
//! The wire envelope is `Result<Vec<u8>, String>` — server-side `Ok` is the
//! consensus-encoded response, `Err` is a description string. Both
//! [`request`] and [`handle_request`] bake the envelope in: callers
//! supply/return the typed response struct, the helpers handle the
//! envelope wrap/unwrap.

use std::sync::Arc;

use anyhow::{Context, anyhow};
use futures::TryFutureExt;
use iroh::endpoint::Connection;
use iroh::{Endpoint, PublicKey};
use picomint_encoding::{Decodable, Encodable};
use tokio::sync::Semaphore;
use tracing::warn;

/// ALPN identifier for picomint RPC. All picomint nodes — guardians and
/// gateways alike — speak the same ALPN; the demux happens at the
/// method-enum layer.
pub const ALPN: &[u8] = b"picomint";

/// Maximum on-the-wire payload size for a single request or response.
pub const MAX_BYTES: usize = 100_000_000;

/// Open a fresh iroh connection to `node_id`, send `request`, read the
/// response, close. The wire envelope (`Result<Vec<u8>, String>`) is
/// unwrapped here — the caller gets back the consensus-decoded `Resp`
/// directly, or an `anyhow::Error` carrying the server-side error string.
pub async fn request<Req: Encodable, Resp: Decodable>(
    endpoint: &Endpoint,
    node_id: PublicKey,
    request: Req,
) -> anyhow::Result<Resp> {
    let connection = endpoint
        .connect(node_id, ALPN)
        .await
        .context("Connection failed")?;

    let response = request_on_connection(&connection, request).await;

    connection.close(0u32.into(), b"");

    response
}

/// Send one request over an existing, kept-alive [`Connection`] by opening a
/// fresh bi stream on it. The connection is left open for reuse — the caller
/// owns its lifecycle. The federation client multiplexes every per-peer
/// request over a single pooled connection this way; the server's
/// [`handle_request`] accept loop serves them as independent streams.
pub async fn request_on_connection<Req: Encodable, Resp: Decodable>(
    connection: &Connection,
    request: Req,
) -> anyhow::Result<Resp> {
    let request_bytes = request.consensus_encode_to_vec();

    let (mut sink, mut stream) = connection.open_bi().await.context("Failed to open bi")?;

    sink.write_all(&request_bytes)
        .await
        .context("Failed to write request")?;

    sink.finish().context("Failed to finish send stream")?;

    let response = stream
        .read_to_end(MAX_BYTES)
        .await
        .context("Failed to read response")?;

    let envelope = <Result<Vec<u8>, String>>::consensus_decode(&response)
        .context("Failed to decode response envelope")?;

    let bytes = envelope.map_err(|e| anyhow!("Server error: {e}"))?;

    Resp::consensus_decode(&bytes).context("Failed to decode response payload")
}

/// Run the accept loop for an iroh [`Endpoint`], spawning one task per
/// connection that drives [`handle_request`] with `handler`. `request_limit`
/// caps in-flight requests across all connections. Returns when the endpoint
/// stops accepting (clean shutdown).
pub async fn run_accept_loop<R, F, T>(endpoint: Endpoint, request_limit: usize, handler: F)
where
    R: Decodable + Send + 'static,
    F: Fn(R) -> T + Clone + Send + 'static,
    T: Future<Output = Result<Vec<u8>, String>> + Send + 'static,
{
    let request_limit = Arc::new(Semaphore::new(request_limit));

    while let Some(incoming) = endpoint.accept().await {
        tokio::spawn(
            handle_incoming(incoming, request_limit.clone(), handler.clone())
                .inspect_err(|e| warn!(?e, "iroh request failed")),
        );
    }
}

async fn handle_incoming<R, F, T>(
    incoming: iroh::endpoint::Incoming,
    request_limit: Arc<Semaphore>,
    handler: F,
) -> anyhow::Result<()>
where
    R: Decodable + Send + 'static,
    F: Fn(R) -> T + Clone + Send + 'static,
    T: Future<Output = Result<Vec<u8>, String>> + Send + 'static,
{
    let connection = incoming
        .accept()
        .context("Failed to accept incoming")?
        .await?;

    handle_request(connection, request_limit, handler).await
}

/// Serve a kept-alive iroh connection: accept bi streams in a loop, handling
/// each as one independent request, until the peer closes the connection.
/// Connections are pooled and reused by clients, so a single connection may
/// carry many requests over its lifetime. `request_limit` caps in-flight
/// requests across all connections; each stream is handled on its own task.
/// The handler returns `Result<Vec<u8>, String>` — bytes are the
/// consensus-encoded response, error is a description string; the wire
/// envelope wrap is handled here.
pub async fn handle_request<Req, F, Fut>(
    connection: Connection,
    request_limit: Arc<Semaphore>,
    handler: F,
) -> anyhow::Result<()>
where
    Req: Decodable + Send + 'static,
    F: Fn(Req) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Result<Vec<u8>, String>> + Send + 'static,
{
    loop {
        // `accept_bi` errors once the peer closes (or the connection drops) —
        // a normal end-of-life for a pooled connection, not a failure.
        let Ok((mut send_stream, mut recv_stream)) = connection.accept_bi().await else {
            return Ok(());
        };

        let permit = request_limit
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore should not be closed");

        let handler = handler.clone();

        tokio::spawn(async move {
            let _permit = permit;

            let result: anyhow::Result<()> = async move {
                let request_bytes = recv_stream.read_to_end(MAX_BYTES).await?;

                let request = Req::consensus_decode(&request_bytes)?;

                let response = handler(request).await;

                let response_bytes = response.consensus_encode_to_vec();

                send_stream.write_all(&response_bytes).await?;

                send_stream.finish()?;

                Ok(())
            }
            .await;

            if let Err(e) = result {
                warn!(?e, "iroh request stream failed");
            }
        });
    }
}
