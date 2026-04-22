//! Core module system types shared between the server and client sides.
pub mod audit;

use serde::{Deserialize, Serialize};

use crate::Amount;
use crate::ln::methods::LnMethod;
use crate::methods::CoreMethod;
use crate::mint::methods::MintMethod;
use crate::wallet::methods::WalletMethod;
use picomint_encoding::{Decodable, Encodable};

#[derive(Debug, PartialEq, Eq)]
pub struct InputMeta {
    pub amount: TransactionItemAmounts,
    pub pub_key: secp256k1::PublicKey,
}

/// Information about the amount represented by an input or output.
///
/// * For **inputs** the amount is funding the transaction while the fee is
///   consuming funding
/// * For **outputs** the amount and the fee consume funding
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct TransactionItemAmounts {
    pub amount: Amount,
    pub fee: Amount,
}

/// The wire method dispatched to a guardian over iroh. Each variant carries
/// the concrete request for its module; the response type is determined by
/// the variant the client sent.
#[derive(Debug, Clone, Encodable, Decodable)]
pub enum ApiMethod {
    Core(CoreMethod),
    Mint(MintMethod),
    Ln(LnMethod),
    Wallet(WalletMethod),
}

pub const PICOMINT_ALPN: &[u8] = b"picomint";

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

#[derive(Debug, Clone, Serialize, Deserialize, Encodable, Decodable, thiserror::Error)]
#[error("{code} {message}")]
pub struct ApiError {
    pub code: u32,
    pub message: String,
}

impl ApiError {
    pub fn not_found(message: String) -> Self {
        Self { code: 404, message }
    }

    pub fn bad_request(message: String) -> Self {
        Self { code: 400, message }
    }
}
