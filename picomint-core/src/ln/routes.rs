//! Gateway public HTTP routes. These are axum paths — leading slash is
//! required by `Router::route`, so it belongs to the constant. Joiners
//! trim the base's trailing slash only:
//! `format!("{}{}", base.trim_end_matches('/'), route)`.

pub const ROUTE_CREATE_BOLT11_INVOICE: &str = "/create-bolt11-invoice";
pub const ROUTE_VERIFY_BOLT11_PREIMAGE: &str = "/verify-bolt11-preimage";
pub const ROUTE_GATEWAY_INFO: &str = "/gateway-info";
pub const ROUTE_SEND_PAYMENT: &str = "/send-payment";
