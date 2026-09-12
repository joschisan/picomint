//! The daemon side of the admin socket: [`serve`] binds
//! `{DATA_DIR}/cli.sock` and returns the future that runs an axum router
//! on it, and [`CliError`] is what its handlers fail with. The CLI side is
//! `picomint-cli-client`, which spells the socket filename out too — a
//! mismatch fails the first command.

use std::fs::{Permissions, remove_file, set_permissions};
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener as StdUnixListener;
use std::path::Path;

use anyhow::{Context, Result};
use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use tokio::net::UnixListener;

/// The daemon binds and the CLI connects at `{DATA_DIR}/{CLI_SOCKET_FILENAME}`.
pub const CLI_SOCKET_FILENAME: &str = "cli.sock";

/// What an admin handler fails with: a status code and the message the
/// CLI prints.
#[derive(Debug)]
pub struct CliError {
    pub code: StatusCode,
    pub error: String,
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error)
    }
}

impl std::error::Error for CliError {}

impl CliError {
    pub fn bad_request(error: impl std::fmt::Display) -> Self {
        Self {
            code: StatusCode::BAD_REQUEST,
            error: error.to_string(),
        }
    }

    pub fn internal(error: impl std::fmt::Display) -> Self {
        Self {
            code: StatusCode::INTERNAL_SERVER_ERROR,
            error: error.to_string(),
        }
    }
}

impl IntoResponse for CliError {
    fn into_response(self) -> axum::response::Response {
        (self.code, self.error).into_response()
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
