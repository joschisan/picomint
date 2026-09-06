//! The daemon side of the admin socket: [`serve`] binds
//! `{DATA_DIR}/cli.sock` and runs an axum router on it, and [`CliError`]
//! is what its handlers fail with. The CLI side is `picomint-cli-client`,
//! which spells the socket filename out too — a mismatch fails the first
//! command.

use std::path::Path;

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

/// Bind the admin socket at `{data_dir}/{CLI_SOCKET_FILENAME}` and
/// serve `router` on it until the process exits. A stale socket from a
/// previous (crashed) run is unlinked before binding.
pub async fn serve(data_dir: &Path, router: Router) {
    let socket_path = data_dir.join(CLI_SOCKET_FILENAME);

    std::fs::remove_file(&socket_path).ok();

    let listener = UnixListener::bind(&socket_path).expect("Failed to bind the admin socket");

    axum::serve(listener, router.into_make_service())
        .await
        .expect("Admin socket server failed");
}
