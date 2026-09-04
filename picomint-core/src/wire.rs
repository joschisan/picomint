//! Static wire enums for the fixed module set: ecash + onchain + lightning.

use crate::ecash::{EcashInput, EcashInputError, EcashOutput, EcashOutputError};
use crate::lightning::{
    LightningInput, LightningInputError, LightningOutput, LightningOutputError,
};
use crate::onchain::{
    OnchainConsensusItem, OnchainInput, OnchainInputError, OnchainOutput, OnchainOutputError,
};
use picomint_encoding::{Decodable, Encodable};
use thiserror::Error;

#[derive(Debug, Clone, Eq, PartialEq, Hash, Encodable, Decodable)]
pub enum Input {
    Ecash(EcashInput),
    Onchain(OnchainInput),
    Lightning(LightningInput),
}

impl From<EcashInput> for Input {
    fn from(v: EcashInput) -> Self {
        Self::Ecash(v)
    }
}

impl From<LightningInput> for Input {
    fn from(v: LightningInput) -> Self {
        Self::Lightning(v)
    }
}

impl From<OnchainInput> for Input {
    fn from(v: OnchainInput) -> Self {
        Self::Onchain(v)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Encodable, Decodable)]
pub enum Output {
    Ecash(EcashOutput),
    Onchain(OnchainOutput),
    Lightning(Box<LightningOutput>),
}

impl From<EcashOutput> for Output {
    fn from(v: EcashOutput) -> Self {
        Self::Ecash(v)
    }
}

impl From<LightningOutput> for Output {
    fn from(v: LightningOutput) -> Self {
        Self::Lightning(Box::new(v))
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
    #[error("Ecash input error: {0}")]
    Ecash(EcashInputError),
    #[error("Onchain input error: {0}")]
    Onchain(OnchainInputError),
    #[error("Lightning input error: {0}")]
    Lightning(LightningInputError),
}

impl From<EcashInputError> for InputError {
    fn from(v: EcashInputError) -> Self {
        Self::Ecash(v)
    }
}

impl From<LightningInputError> for InputError {
    fn from(v: LightningInputError) -> Self {
        Self::Lightning(v)
    }
}

impl From<OnchainInputError> for InputError {
    fn from(v: OnchainInputError) -> Self {
        Self::Onchain(v)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Encodable, Decodable, Error)]
pub enum OutputError {
    #[error("Ecash output error: {0}")]
    Ecash(EcashOutputError),
    #[error("Onchain output error: {0}")]
    Onchain(OnchainOutputError),
    #[error("Lightning output error: {0}")]
    Lightning(LightningOutputError),
}

impl From<EcashOutputError> for OutputError {
    fn from(v: EcashOutputError) -> Self {
        Self::Ecash(v)
    }
}

impl From<LightningOutputError> for OutputError {
    fn from(v: LightningOutputError) -> Self {
        Self::Lightning(v)
    }
}

impl From<OnchainOutputError> for OutputError {
    fn from(v: OnchainOutputError) -> Self {
        Self::Onchain(v)
    }
}
