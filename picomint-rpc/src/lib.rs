//! iroh RPC primitives shared by picomint client and server.
//!
//! One request = one bidirectional stream. Connections are kept alive and
//! reused: the mint client holds a pooled connection per node (see
//! `picomint-client`'s `MintApi`) and multiplexes every request as a
//! fresh bi stream over it via [`request_on_connection`], paying the QUIC
//! handshake and hole-punched path once rather than per request. [`request`]
//! remains a one-shot convenience (connect → one request → close) for
//! callers without a pool, e.g. fetching the config from an invite code.
//!
//! Server-side, [`handle_request`] serves a connection by accepting bi
//! streams in a loop until the node closes, handling each as one request
//! (accept_bi → decode → handler → encode → finish) on its own task.
//!
//! The wire envelope is `Result<Vec<u8>, String>` — server-side `Ok` is the
//! consensus-encoded response, `Err` is a description string. Both
//! [`request`] and [`handle_request`] bake the envelope in: callers
//! supply/return the typed response struct, the helpers handle the
//! envelope wrap/unwrap.

pub mod api;
pub mod connection;
pub mod query;

use std::time::Duration;

use anyhow::{Context, anyhow};
use futures::TryFutureExt;
use iroh::endpoint::{Connection, IdleTimeout, QuicTransportConfig};
use iroh::{Endpoint, PublicKey};
use picomint_encoding::{Decodable, Encodable};
use tracing::warn;

/// ALPN identifier for picomint RPC. All picomint nodes — nodes and
/// gateways alike — speak the same ALPN; the demux happens at the
/// method-enum layer.
pub const ALPN: &[u8] = b"picomint";

/// Maximum on-the-wire payload size for a single request or response.
pub const MAX_BYTES: usize = 100_000_000;

/// QUIC transport config for every picomint endpoint: a 1s keep-alive under
/// a 5s idle timeout, so a dead connection surfaces within ~5s instead of
/// the 30s QUIC default. The negotiated idle timeout is the minimum of both
/// sides', so the daemons setting this caps detection latency for clients
/// whose endpoints keep iroh's defaults. The path-level settings stay
/// untouched — iroh tunes those for hole punching.
pub fn transport_config() -> QuicTransportConfig {
    let idle_timeout = IdleTimeout::try_from(Duration::from_secs(5)).expect("valid timeout");

    QuicTransportConfig::builder()
        .keep_alive_interval(Duration::from_secs(1))
        .max_idle_timeout(Some(idle_timeout))
        .build()
}

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
/// owns its lifecycle. The mint client multiplexes every per-node
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
/// connection that drives [`handle_request`] with `handler`. Returns when
/// the endpoint stops accepting (clean shutdown).
pub async fn run_accept_loop<R, F, T>(endpoint: Endpoint, handler: F)
where
    R: Decodable + Send + 'static,
    F: Fn(R) -> T + Clone + Send + 'static,
    T: Future<Output = Result<Vec<u8>, String>> + Send + 'static,
{
    while let Some(incoming) = endpoint.accept().await {
        tokio::spawn(
            handle_incoming(incoming, handler.clone())
                .inspect_err(|e| warn!(?e, "iroh request failed")),
        );
    }
}

async fn handle_incoming<R, F, T>(
    incoming: iroh::endpoint::Incoming,
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

    handle_request(connection, handler).await
}

/// Serve a kept-alive iroh connection: accept bi streams in a loop, handling
/// each as one independent request on its own task, until the node closes
/// the connection. Connections are pooled and reused by clients, so a single
/// connection may carry many requests over its lifetime. The handler returns
/// `Result<Vec<u8>, String>` — bytes are the consensus-encoded response,
/// error is a description string; the wire envelope wrap is handled here.
pub async fn handle_request<Req, F, Fut>(connection: Connection, handler: F) -> anyhow::Result<()>
where
    Req: Decodable + Send + 'static,
    F: Fn(Req) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Result<Vec<u8>, String>> + Send + 'static,
{
    loop {
        // `accept_bi` errors once the node closes (or the connection drops) —
        // a normal end-of-life for a pooled connection, not a failure.
        let Ok((mut send_stream, mut recv_stream)) = connection.accept_bi().await else {
            return Ok(());
        };

        let handler = handler.clone();

        tokio::spawn(async move {
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
