//! Core module system types shared between the server and client sides.
pub mod audit;

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::Amount;
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

/// Type-erased API request: `params` carries the consensus-encoded parameter
/// bytes, which the endpoint decodes into its concrete `Param` type.
#[derive(Debug, Clone, Encodable, Decodable)]
pub struct ApiRequestErased {
    pub params: Vec<u8>,
}

impl Default for ApiRequestErased {
    fn default() -> Self {
        Self::new(())
    }
}

impl ApiRequestErased {
    pub fn new<T: Encodable>(params: T) -> Self {
        Self {
            params: params.consensus_encode_to_vec(),
        }
    }

    pub fn to_typed<T: Decodable>(&self) -> std::io::Result<T> {
        T::consensus_decode_exact(&self.params)
    }
}

#[derive(Debug, Clone, Encodable, Decodable)]
pub enum ApiMethod {
    Core(String),
    Mint(String),
    Ln(String),
    Wallet(String),
}

impl fmt::Display for ApiMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Core(s) => f.write_fmt(format_args!("core/{s}")),
            Self::Mint(s) => f.write_fmt(format_args!("mint/{s}")),
            Self::Ln(s) => f.write_fmt(format_args!("ln/{s}")),
            Self::Wallet(s) => f.write_fmt(format_args!("wallet/{s}")),
        }
    }
}

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct IrohApiRequest {
    pub method: ApiMethod,
    pub request: ApiRequestErased,
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
