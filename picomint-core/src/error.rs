//! The code an error crosses the admin socket with.
//!
//! Every error a daemon hands back over its admin socket is a `thiserror`
//! enum deriving [`ErrorCode`]: the variant names the code, the `#[error]`
//! message explains it. The daemon puts both in the JSON error body, and
//! the CLI's `--help` lists every code a command fails with, from the same
//! enum, so the list cannot drift from what the daemon returns. The derive
//! is [`picomint_derive::ErrorCode`], re-exported here.

pub use picomint_derive::ErrorCode;

/// An error enum whose variants are stable codes.
pub trait ErrorCode {
    /// Every variant as `(code, message)`: the variant name in snake_case
    /// and its `#[error]` message as written, format placeholders included.
    const CODES: &'static [(&'static str, &'static str)];

    /// This value's variant name in snake_case.
    fn code(&self) -> &'static str;
}
