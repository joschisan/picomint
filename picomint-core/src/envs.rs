use picomint_logging::LOG_CORE;
use tracing::warn;

/// Env var set by the integration test harness when daemons run as subprocesses
/// of `picomint-integration-tests`.
pub const IN_TEST_ENV: &str = "IN_TEST_ENV";

/// Check if env variable is set and not equal `0` or `false` which are common
/// ways to disable something.
pub fn is_env_var_set(var: &str) -> bool {
    let Some(val) = std::env::var_os(var) else {
        return false;
    };
    match val.as_encoded_bytes() {
        b"0" | b"false" => false,
        b"1" | b"true" => true,
        _ => {
            warn!(
                target: LOG_CORE,
                %var,
                val = %val.to_string_lossy(),
                "Env var value invalid is invalid and ignored, assuming `true`"
            );
            true
        }
    }
}

/// Use to detect if running in a test environment, either `cargo test`,
/// `cargo nextest`, or the integration test harness.
pub fn is_running_in_test_env() -> bool {
    cfg!(test) || is_env_var_set("NEXTEST") || is_env_var_set(IN_TEST_ENV)
}
