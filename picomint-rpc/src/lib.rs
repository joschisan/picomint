//! One-shot iroh RPC primitives shared by picomint client and server.
//!
//! Each request opens a fresh iroh [`Connection`], sends one bidirectional
//! stream, then drops the connection. iroh's [`Endpoint`] caches per-remote
//! address + path state across calls (60s actor idle timeout), so warm
//! reconnects skip discovery and pay only the QUIC handshake.
//!
//! Server-side, [`handle_request`] drives one connection through the one-shot
//! lifecycle: accept_bi → decode → handler → encode → finish → wait for the
//! peer to close. Closing before the peer has consumed application data
//! risks the QUIC stack dropping bytes (per `Connection::close` docs); the
//! receiver-driven close mirrors iroh's `examples/echo-no-router.rs`.
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

    connection.close(0u32.into(), b"");

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
    R: Decodable + 'static,
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
    R: Decodable,
    F: FnOnce(R) -> T,
    T: Future<Output = Result<Vec<u8>, String>>,
{
    let connection = incoming
        .accept()
        .context("Failed to accept incoming")?
        .await?;

    handle_request(connection, request_limit, handler).await
}

/// Drive one accepted iroh connection through the one-shot RPC lifecycle.
/// The handler returns `Result<Vec<u8>, String>` — bytes are the
/// consensus-encoded response, error is a description string. The wire
/// envelope wrap is handled here.
pub async fn handle_request<Req, F, Fut>(
    connection: Connection,
    request_limit: Arc<Semaphore>,
    handler: F,
) -> anyhow::Result<()>
where
    Req: Decodable,
    F: FnOnce(Req) -> Fut,
    Fut: Future<Output = Result<Vec<u8>, String>>,
{
    let _permit = request_limit
        .acquire_owned()
        .await
        .expect("semaphore should not be closed");

    let (mut send_stream, mut recv_stream) = connection.accept_bi().await?;

    let request_bytes = recv_stream.read_to_end(MAX_BYTES).await?;

    let request = Req::consensus_decode(&request_bytes)?;

    let response = handler(request).await;

    let response_bytes = response.consensus_encode_to_vec();

    send_stream.write_all(&response_bytes).await?;

    send_stream.finish()?;

    connection.closed().await;

    Ok(())
}
