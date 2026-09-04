//! Static wire enums for the fixed module set: ecash + wallet + ln.

use crate::ln::{LightningInput, LightningInputError, LightningOutput, LightningOutputError};
use crate::ecash::{ECashInput, ECashInputError, ECashOutput, ECashOutputError};
use crate::wallet::{
    WalletConsensusItem, WalletInput, WalletInputError, WalletOutput, WalletOutputError,
};
use picomint_encoding::{Decodable, Encodable};
use thiserror::Error;

#[derive(Debug, Clone, Eq, PartialEq, Hash, Encodable, Decodable)]
pub enum Input {
    ECash(ECashInput),
    Wallet(WalletInput),
    Ln(LightningInput),
}

impl From<ECashInput> for Input {
    fn from(v: ECashInput) -> Self {
        Self::ECash(v)
    }
}

impl From<LightningInput> for Input {
    fn from(v: LightningInput) -> Self {
        Self::Ln(v)
    }
}

impl From<WalletInput> for Input {
    fn from(v: WalletInput) -> Self {
        Self::Wallet(v)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Encodable, Decodable)]
pub enum Output {
    ECash(ECashOutput),
    Wallet(WalletOutput),
    Ln(Box<LightningOutput>),
}

impl From<ECashOutput> for Output {
    fn from(v: ECashOutput) -> Self {
        Self::ECash(v)
    }
}

impl From<LightningOutput> for Output {
    fn from(v: LightningOutput) -> Self {
        Self::Ln(Box::new(v))
    }
}

impl From<WalletOutput> for Output {
    fn from(v: WalletOutput) -> Self {
        Self::Wallet(v)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Encodable, Decodable)]
pub enum ModuleConsensusItem {
    Wallet(WalletConsensusItem),
}

impl From<WalletConsensusItem> for ModuleConsensusItem {
    fn from(v: WalletConsensusItem) -> Self {
        Self::Wallet(v)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Encodable, Decodable, Error)]
pub enum InputError {
    #[error("ECash input error: {0}")]
    ECash(ECashInputError),
    #[error("Wallet input error: {0}")]
    Wallet(WalletInputError),
    #[error("Lightning input error: {0}")]
    Ln(LightningInputError),
}

impl From<ECashInputError> for InputError {
    fn from(v: ECashInputError) -> Self {
        Self::ECash(v)
    }
}

impl From<LightningInputError> for InputError {
    fn from(v: LightningInputError) -> Self {
        Self::Ln(v)
    }
}

impl From<WalletInputError> for InputError {
    fn from(v: WalletInputError) -> Self {
        Self::Wallet(v)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Encodable, Decodable, Error)]
pub enum OutputError {
    #[error("ECash output error: {0}")]
    ECash(ECashOutputError),
    #[error("Wallet output error: {0}")]
    Wallet(WalletOutputError),
    #[error("Lightning output error: {0}")]
    Ln(LightningOutputError),
}

impl From<ECashOutputError> for OutputError {
    fn from(v: ECashOutputError) -> Self {
        Self::ECash(v)
    }
}

impl From<LightningOutputError> for OutputError {
    fn from(v: LightningOutputError) -> Self {
        Self::Ln(v)
    }
}

impl From<WalletOutputError> for OutputError {
    fn from(v: WalletOutputError) -> Self {
        Self::Wallet(v)
    }
}
