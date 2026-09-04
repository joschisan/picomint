//! Core module system types shared between the server and client sides.
pub mod audit;

use serde::{Deserialize, Serialize};

use crate::ln::methods::LnMethod;
use crate::methods::CoreMethod;
use crate::ecash::methods::ECashMethod;
use crate::onchain::methods::OnchainMethod;
use picomint_encoding::{Decodable, Encodable};

/// The wire method dispatched to a guardian over iroh. Each variant carries
/// the concrete request for its module; the response type is determined by
/// the variant the client sent.
#[derive(Debug, Clone, Encodable, Decodable)]
pub enum Method {
    Core(CoreMethod),
    ECash(ECashMethod),
    Onchain(OnchainMethod),
    Ln(LnMethod),
}

/// Authentication secret used to verify guardian admin API requests.
///
/// The inner value is private to prevent timing leaks via direct comparison.
/// Use [`Self::verify`] for authentication checks. No `Debug` impl — the
/// plaintext must never end up in a log. [`Self::as_str`] is a temporary
/// escape hatch for I/O that still needs the plaintext value and should be
/// removed once passwords are hashed at rest.
#[derive(Clone, Serialize, Deserialize, Encodable, Decodable)]
pub struct ApiAuth(String);

impl ApiAuth {
    pub fn new(s: String) -> Self {
        Self(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn verify(&self, password: &str) -> bool {
        use subtle::ConstantTimeEq as _;
        bool::from(self.0.as_bytes().ct_eq(password.as_bytes()))
    }
}
