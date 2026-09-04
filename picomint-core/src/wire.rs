//! Static wire enums for the fixed module set: ecash + onchain + ln.

use crate::ln::{LightningInput, LightningInputError, LightningOutput, LightningOutputError};
use crate::ecash::{ECashInput, ECashInputError, ECashOutput, ECashOutputError};
use crate::onchain::{
    OnchainConsensusItem, OnchainInput, OnchainInputError, OnchainOutput, OnchainOutputError,
};
use picomint_encoding::{Decodable, Encodable};
use thiserror::Error;

#[derive(Debug, Clone, Eq, PartialEq, Hash, Encodable, Decodable)]
pub enum Input {
    ECash(ECashInput),
    Onchain(OnchainInput),
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

impl From<OnchainInput> for Input {
    fn from(v: OnchainInput) -> Self {
        Self::Onchain(v)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Encodable, Decodable)]
pub enum Output {
    ECash(ECashOutput),
    Onchain(OnchainOutput),
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

impl From<OnchainOutput> for Output {
    fn from(v: OnchainOutput) -> Self {
        Self::Onchain(v)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Encodable, Decodable)]
pub enum ModuleConsensusItem {
    Onchain(OnchainConsensusItem),
}

impl From<OnchainConsensusItem> for ModuleConsensusItem {
    fn from(v: OnchainConsensusItem) -> Self {
        Self::Onchain(v)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Encodable, Decodable, Error)]
pub enum InputError {
    #[error("ECash input error: {0}")]
    ECash(ECashInputError),
    #[error("Onchain input error: {0}")]
    Onchain(OnchainInputError),
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

impl From<OnchainInputError> for InputError {
    fn from(v: OnchainInputError) -> Self {
        Self::Onchain(v)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Encodable, Decodable, Error)]
pub enum OutputError {
    #[error("ECash output error: {0}")]
    ECash(ECashOutputError),
    #[error("Onchain output error: {0}")]
    Onchain(OnchainOutputError),
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

impl From<OnchainOutputError> for OutputError {
    fn from(v: OnchainOutputError) -> Self {
        Self::Onchain(v)
    }
}
