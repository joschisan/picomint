//! The CLI side of the admin socket every picomint daemon serves: HTTP
//! over a Unix socket at `{DATA_DIR}/cli.sock`, JSON in, JSON out.
//! [`request`] posts a payload to a route and returns the reply, or the
//! [`RequestError`] the CLI exits with; [`schema`] renders a response
//! type's JSON Schema for a command's `--help`, [`schema_fallible`] the
//! codes it fails with as well. The
//! daemon side is `picomint-cli-server`; routes and payload types are each
//! daemon's own and live in its `*-cli-core` crate. The socket filename
//! is spelled out on both sides — a mismatch fails the first command.

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};

use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper::body::Bytes;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use picomint_core::error::ErrorCode;
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::net::UnixStream;
use tower_service::Service;

/// The daemon binds and the CLI connects at `{DATA_DIR}/{CLI_SOCKET_FILENAME}`.
pub const CLI_SOCKET_FILENAME: &str = "cli.sock";

/// Shown at the end of every CLI's top-level `--help`: the rules for an
/// agent driving it, stated where the agent reads them.
pub const FOOTER: &str = "\
Commands marked (secret) print key material, and whatever an agent reads \
ends up in its context and transcript. Rules for an agent: run a (secret) \
command only when the operator asks; run it with stdout redirected into a \
file; never read that file; open it for the operator if asked.\n\n\
Errors print one JSON object on stderr, {\"code\": ..., \"error\": ...}: the \
code is what to branch on, the error what to tell the operator. The exit \
code is 1 for a request the daemon refused, 2 for a usage error and 3 when \
the daemon is unreachable. A command's --help lists every code it fails \
with.";

/// Why a command failed, and how the CLI exits on it: one JSON object on
/// stderr, `{"code": ..., "error": ...}`, and an exit code by class.
#[derive(Debug)]
pub enum RequestError {
    /// The daemon did not answer: no socket at the data dir, or the
    /// connection failed. Exit code 3.
    Unreachable(String),
    /// The daemon answered with an error body; `code` and `error` are its.
    /// Exit code 1.
    Rejected { code: String, error: String },
    /// The daemon's reply was not the JSON expected. Exit code 1.
    Malformed(String),
    /// The command's input could not be read. Exit code 2.
    Usage(String),
}

impl RequestError {
    /// Print the error on stderr and exit with its class's code.
    pub fn exit(self) -> ! {
        let (code, error, exit) = match self {
            RequestError::Unreachable(error) => ("daemon_unreachable".to_string(), error, 3),
            RequestError::Rejected { code, error } => (code, error, 1),
            RequestError::Malformed(error) => ("malformed".to_string(), error, 1),
            RequestError::Usage(error) => ("usage".to_string(), error, 2),
        };

        eprintln!("{}", serde_json::json!({ "code": code, "error": error }));

        std::process::exit(exit)
    }
}

/// The body a daemon answers a refused request with.
#[derive(Deserialize)]
struct ErrorBody {
    code: String,
    error: String,
}

/// Pretty-print a reply the way every CLI does.
pub fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("Cannot serialize")
    );
}

/// The JSON Schema of a command's response, rendered for the tail of its
/// `--help`: every field explained, no daemon needed. Named on the
/// command's variant as `#[command(after_long_help = schema::<Resp>())]`.
pub fn schema<Resp: JsonSchema>() -> String {
    format!(
        "Prints, as JSON Schema:\n{}",
        serde_json::to_string_pretty(&schema_for!(Resp)).expect("a schema serializes")
    )
}

/// [`schema`] followed by every code the command fails with, from the
/// error enum the daemon's handler returns, so help and daemon cannot
/// disagree.
pub fn schema_fallible<Resp: JsonSchema, Err: ErrorCode>() -> String {
    let codes = Err::codes()
        .iter()
        .map(|entry| format!("{}: {}", entry.0, entry.1))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "{}\n\nFails with, as the code of the JSON error on stderr:\n{codes}",
        schema::<Resp>()
    )
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
pub async fn request<R: Serialize>(
    data_dir: &Path,
    route: &str,
    payload: R,
) -> Result<Value, RequestError> {
    let socket_path = data_dir.join(CLI_SOCKET_FILENAME);
    let connector = UnixConnector {
        path: socket_path.clone(),
    };
    let client = Client::builder(TokioExecutor::new()).build(connector);

    let body_bytes = serde_json::to_vec(&payload).expect("a request serializes");
    let uri: hyper::Uri = format!("http://localhost{route}")
        .parse()
        .expect("every route is a valid path");
    let req = Request::post(uri)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body_bytes)))
        .expect("a request with a body builds");

    let resp = client.request(req).await.map_err(|e| {
        RequestError::Unreachable(format!(
            "Failed to POST {route} to the daemon at {}: {e}",
            socket_path.display()
        ))
    })?;

    let status = resp.status();
    let resp_bytes = resp
        .into_body()
        .collect()
        .await
        .map_err(|e| RequestError::Unreachable(format!("The daemon's reply broke off: {e}")))?
        .to_bytes();

    if !status.is_success() {
        return Err(match serde_json::from_slice::<ErrorBody>(&resp_bytes) {
            Ok(body) => RequestError::Rejected {
                code: body.code,
                error: body.error,
            },
            Err(_) => RequestError::Rejected {
                code: if status.is_server_error() {
                    "internal".to_string()
                } else {
                    "bad_request".to_string()
                },
                error: String::from_utf8_lossy(&resp_bytes).into_owned(),
            },
        });
    }

    if resp_bytes.is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_slice(&resp_bytes)
            .map_err(|e| RequestError::Malformed(format!("The daemon's reply is not JSON: {e}")))
    }
}
