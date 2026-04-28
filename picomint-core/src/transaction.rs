//! Wire-level transaction and consensus-item types shared between client and
//! server. Previously lived in `picomint-core::transaction` / `epoch.rs`;
//! moved here with the module-system rip so we can reference static module
//! Input/Output/ConsensusItem enums without creating a cycle through
//! picomint-core.

use bitcoin::hashes::Hash as _;
use picomint_encoding::{Decodable, Encodable};
use thiserror::Error;

use crate::TransactionId;
use crate::wire;

/// An atomic value transfer operation within the Picomint system and consensus.
///
/// The mint enforces that the total value of the outputs equals the total value
/// of the inputs, to prevent creating funds out of thin air. In some cases, the
/// value of the inputs and outputs can both be 0 e.g. when creating an offer to
/// a Lightning Gateway.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Encodable, Decodable)]
pub struct Transaction {
    pub inputs: Vec<wire::Input>,
    pub outputs: Vec<wire::Output>,
    pub signatures: Vec<crate::secp256k1::schnorr::Signature>,
}

impl Transaction {
    pub const MAX_TX_SIZE: usize = crate::config::ALEPH_BFT_UNIT_BYTE_LIMIT - 32;

    pub fn tx_hash(&self) -> TransactionId {
        Self::tx_hash_from_parts(&self.inputs, &self.outputs)
    }

    pub fn tx_hash_from_parts(inputs: &[wire::Input], outputs: &[wire::Output]) -> TransactionId {
        TransactionId((inputs, outputs).consensus_hash_sha256())
    }

    pub fn validate_signatures(
        &self,
        pub_keys: &[crate::secp256k1::XOnlyPublicKey],
    ) -> Result<(), TransactionError> {
        use crate::secp256k1;

        if pub_keys.len() != self.signatures.len() {
            return Err(TransactionError::InvalidWitnessLength);
        }

        let txid = self.tx_hash();
        let msg = secp256k1::Message::from_digest(*txid.0.as_byte_array());

        for (pk, signature) in pub_keys.iter().zip(&self.signatures) {
            if secp256k1::global::SECP256K1
                .verify_schnorr(signature, &msg, pk)
                .is_err()
            {
                return Err(TransactionError::InvalidSignature);
            }
        }

        Ok(())
    }
}

#[derive(Debug, Error, Encodable, Decodable, Clone, Eq, PartialEq)]
pub enum TransactionError {
    #[error("The transaction has no inputs")]
    EmptyInputs,
    #[error("The transaction has no outputs")]
    EmptyOutputs,
    #[error("The transaction is underfunded")]
    Underfunded,
    #[error("Amount arithmetic overflowed u64 msats")]
    Overflow,
    #[error("The transaction did not have the correct number of signatures")]
    InvalidWitnessLength,
    #[error("The transaction's signature is invalid")]
    InvalidSignature,
    #[error("The transaction had an invalid input: {}", .0)]
    Input(wire::InputError),
    #[error("The transaction had an invalid output: {}", .0)]
    Output(wire::OutputError),
}

/// All the items that may be produced during a consensus epoch.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Encodable, Decodable)]
pub enum ConsensusItem {
    /// Threshold sign the epoch history for verification via the API
    Transaction(Transaction),
    /// Any data that modules require consensus on
    Module(wire::ModuleConsensusItem),
}
