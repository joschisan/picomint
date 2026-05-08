use std::path::Path;

use crate::config::ServerConfig;

/// Filename of the on-disk guardian config snapshot, kept alongside the redb
/// database in the data dir. The contents are byte-for-byte identical to what
/// the `picomint-server-cli config` admin command emits, so backup tooling
/// (Start9 v4, ad-hoc rsync, etc.) can capture and restore guardian state by
/// copying just this single file.
pub const SERVER_CONFIG_FILENAME: &str = "config.json";

/// Persist `cfg` as `<data_dir>/config.json`.
///
/// Idempotent: callers can invoke this on every startup without worrying
/// about over-writing — the federation config is write-once after DKG.
/// Panics on I/O or serialization failure: a daemon that can't persist its
/// config snapshot is in an unrecoverable state and should crash loudly.
pub fn write_server_config_file(data_dir: &Path, cfg: &ServerConfig) {
    let json = serde_json::to_vec_pretty(cfg).expect("ServerConfig serialization failed");

    std::fs::write(data_dir.join(SERVER_CONFIG_FILENAME), json)
        .expect("Failed to write config.json");
}

/// Read `<data_dir>/config.json` if present. Returns `None` only when the
/// file is missing (the normal pre-DKG state). Panics on read or parse
/// failure: a corrupt config.json is operator-fixable, not auto-recoverable.
pub fn load_server_config_file(data_dir: &Path) -> Option<ServerConfig> {
    let path = data_dir.join(SERVER_CONFIG_FILENAME);

    if !path.exists() {
        return None;
    }

    let bytes = std::fs::read(&path).expect("Failed to read config.json");

    Some(serde_json::from_slice(&bytes).expect("Corrupt config.json"))
}
