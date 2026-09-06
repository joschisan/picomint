//! The CLI side of the admin socket every picomint daemon serves: HTTP
//! over a Unix socket at `{DATA_DIR}/cli.sock`, JSON in, JSON out.
//! [`request`] posts a payload to a route and returns the reply. The
//! daemon side is `picomint-cli-server`; routes and payload types are each
//! daemon's own and live in its `*-cli-core` crate. The socket filename
//! is spelled out on both sides — a mismatch fails the first command.

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};

use anyhow::{Context as _, Result, ensure};
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper::body::Bytes;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use serde::Serialize;
use serde_json::Value;
use tokio::net::UnixStream;
use tower_service::Service;

/// The daemon binds and the CLI connects at `{DATA_DIR}/{CLI_SOCKET_FILENAME}`.
pub const CLI_SOCKET_FILENAME: &str = "cli.sock";

/// Pretty-print a reply the way every CLI does.
pub fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("Cannot serialize")
    );
}

/// Tiny connector that dials a fixed Unix socket path, ignoring the URI
/// entirely. Plugs into `hyper_util::client::legacy::Client` where a TCP
/// connector would normally go.
#[derive(Clone)]
struct UnixConnector {
    path: PathBuf,
}

impl Service<hyper::Uri> for UnixConnector {
    type Response = TokioIo<UnixStream>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<TokioIo<UnixStream>>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: hyper::Uri) -> Self::Future {
        let path = self.path.clone();
        Box::pin(async move { UnixStream::connect(path).await.map(TokioIo::new) })
    }
}

/// POST `payload` as JSON to `route` on the daemon whose data directory
/// is `data_dir`, and return the JSON reply — `Null` for an empty body.
/// A non-success status is an error carrying the daemon's message.
pub async fn request<R: Serialize>(data_dir: &Path, route: &str, payload: R) -> Result<Value> {
    let socket_path = data_dir.join(CLI_SOCKET_FILENAME);
    let connector = UnixConnector {
        path: socket_path.clone(),
    };
    let client = Client::builder(TokioExecutor::new()).build(connector);

    let body_bytes = serde_json::to_vec(&payload)?;
    let uri: hyper::Uri = format!("http://localhost{route}").parse()?;
    let req = Request::post(uri)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body_bytes)))?;

    let resp = client.request(req).await.with_context(|| {
        format!(
            "Failed to POST {route} to the daemon at {}",
            socket_path.display()
        )
    })?;

    let status = resp.status();
    let resp_bytes = resp.into_body().collect().await?.to_bytes();

    ensure!(
        status.is_success(),
        "API error ({}): {}",
        status.as_u16(),
        String::from_utf8_lossy(&resp_bytes)
    );

    if resp_bytes.is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_slice(&resp_bytes).context("Failed to parse the daemon's response")
    }
}
