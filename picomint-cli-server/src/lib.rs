//! The daemon side of the admin socket: [`serve`] binds
//! `{DATA_DIR}/cli.sock` and returns the future that runs an axum router
//! on it, and [`CliError`] is what its handlers fail with: a code from a
//! [`ErrorCode`] enum and a message, as JSON. The CLI side is
//! `picomint-cli-client`, which spells the socket filename out too — a
//! mismatch fails the first command.

use std::fs::{Permissions, remove_file, set_permissions};
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener as StdUnixListener;
use std::path::Path;

use anyhow::{Context, Result};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Json, Router};
use picomint_core::error::ErrorCode;
use serde::Serialize;
use tokio::net::UnixListener;

/// The daemon binds and the CLI connects at `{DATA_DIR}/{CLI_SOCKET_FILENAME}`.
pub const CLI_SOCKET_FILENAME: &str = "cli.sock";

/// What an admin handler fails with, and what the CLI prints: a status,
/// a stable `code` the caller branches on and the `error` message it
/// shows the operator. The body is `{"code": ..., "error": ...}`.
#[derive(Debug, Serialize)]
pub struct CliError {
    #[serde(skip)]
    pub status: StatusCode,
    pub code: &'static str,
    pub error: String,
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.error)
    }
}

impl std::error::Error for CliError {}

impl CliError {
    /// A request the daemon refuses for a reason the caller can act on:
    /// the enum's variant is the code, its message the error. Every
    /// error a handler returns on purpose goes through here, so the CLI's
    /// `--help` can list the codes from the same enum.
    pub fn rejected(error: impl ErrorCode + std::fmt::Display) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: error.code(),
            error: error.to_string(),
        }
    }

    /// A failure nothing typed: an anyhow error reaching a handler, which
    /// is a bug in the daemon rather than a rejection of the request.
    pub fn internal(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal",
            error: error.to_string(),
        }
    }
}

impl IntoResponse for CliError {
    fn into_response(self) -> axum::response::Response {
        (self.status, Json(&self)).into_response()
    }
}

impl From<anyhow::Error> for CliError {
    fn from(e: anyhow::Error) -> Self {
        Self::internal(e)
    }
}

/// Bind the admin socket at `{data_dir}/{CLI_SOCKET_FILENAME}` and return
/// the future that serves `router` on it until the process exits. A stale
/// socket from a previous (crashed) run is unlinked before binding.
///
/// The bind happens here, synchronously, rather than inside the returned
/// future: a daemon spawns the future, so a failure inside it would leave
/// the daemon running with no admin surface, whereas a failure on the
/// daemon's main path ends the process.
///
/// Every route on the socket is full custody and nothing on it
/// authenticates the peer, so the file modes are the only gate. They are
/// set explicitly rather than inherited from the umask: the data dir goes
/// owner-only first, which also covers the database beside the socket and
/// closes the window between bind and chmod.
pub fn serve(data_dir: &Path, router: Router) -> Result<impl Future<Output = ()> + use<>> {
    set_permissions(data_dir, Permissions::from_mode(0o700))
        .with_context(|| format!("Failed to make {} owner-only", data_dir.display()))?;

    let socket_path = data_dir.join(CLI_SOCKET_FILENAME);

    remove_file(&socket_path).ok();

    let listener = StdUnixListener::bind(&socket_path).with_context(|| {
        format!(
            "Failed to bind the admin socket at {}",
            socket_path.display()
        )
    })?;

    set_permissions(&socket_path, Permissions::from_mode(0o600))?;

    listener.set_nonblocking(true)?;

    Ok(async move {
        let listener = UnixListener::from_std(listener).expect("called within a tokio runtime");

        axum::serve(listener, router.into_make_service())
            .await
            .expect("Admin socket server failed");
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn serve_makes_the_data_dir_and_socket_owner_only() {
        let dir = tempfile::tempdir().unwrap();

        std::fs::set_permissions(dir.path(), Permissions::from_mode(0o755)).unwrap();

        let _server = serve(dir.path(), Router::new()).unwrap();

        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;

        assert_eq!(mode(dir.path()), 0o700);
        assert_eq!(mode(&dir.path().join(CLI_SOCKET_FILENAME)), 0o600);
    }

    #[test]
    fn serve_replaces_a_stale_socket() {
        let dir = tempfile::tempdir().unwrap();

        drop(serve(dir.path(), Router::new()).unwrap());

        let _server = serve(dir.path(), Router::new()).unwrap();
    }
}
