//! Guardian wire method names dispatched over Iroh. Not HTTP paths — they
//! are looked up on the receiving side via exact string match (see the
//! `handler!` dispatchers in `picomint-server-daemon::consensus`).

pub const METHOD_CLIENT_CONFIG: &str = "client-config";
pub const METHOD_SUBMIT_TRANSACTION: &str = "submit-transaction";
pub const METHOD_LIVENESS: &str = "liveness";
