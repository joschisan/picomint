//! Wire-level transaction and consensus-item types shared between client and
//! server. Previously lived in `picomint-core::transaction` / `epoch.rs`;
//! moved here with the module-system rip so we can reference static module
//! Input/Output/ConsensusItem enums without creating a cycle through
//! picomint-core.

use bitcoin::hashes::Hash as _;
use picomint_encoding::{Decodable, Encodable};
use thiserror::Error;

use crate::TransactionId;
use crate::version::ConsensusVersion;
use crate::wire;

/// An atomic value transfer operation within the Picomint system and consensus.
///
/// The mint enforces that the total value of the outputs equals the total value
/// of the inputs plus the fees, to prevent creating funds out of thin air.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Encodable, Decodable)]
pub struct Transaction {
    pub inputs: Vec<wire::Input>,
    pub outputs: Vec<wire::Output>,
    pub signatures: Vec<crate::secp256k1::schnorr::Signature>,
}

impl Transaction {
    /// Most inputs a transaction may carry.
    pub const MAX_INPUTS: usize = 1024;

    /// Most outputs a transaction may carry, and so the range every
    /// [`OutPoint::out_idx`] falls in.
    ///
    /// [`OutPoint::out_idx`]: crate::OutPoint::out_idx
    pub const MAX_OUTPUTS: usize = 1024;

    pub fn compute_txid(&self) -> TransactionId {
        Self::compute_txid_from_parts(&self.inputs, &self.outputs)
    }

    pub fn compute_txid_from_parts(
        inputs: &[wire::Input],
        outputs: &[wire::Output],
    ) -> TransactionId {
        TransactionId((inputs, outputs).consensus_hash_sha256())
    }

    pub fn validate_signatures(
        &self,
        pub_keys: &[crate::secp256k1::XOnlyPublicKey],
    ) -> Result<(), TxError> {
        use crate::secp256k1;

        if pub_keys.len() != self.signatures.len() {
            return Err(TxError::InvalidWitnessLength);
        }

        let txid = self.compute_txid();
        let msg = secp256k1::Message::from_digest(*txid.0.as_byte_array());

        for (pk, signature) in pub_keys.iter().zip(&self.signatures) {
            if secp256k1::global::SECP256K1
                .verify_schnorr(signature, &msg, pk)
                .is_err()
            {
                return Err(TxError::InvalidSignature);
            }
        }

        Ok(())
    }
}

#[derive(Debug, Error, Encodable, Decodable, Clone, Eq, PartialEq)]
pub enum TxError {
    #[error("The transaction has no inputs")]
    EmptyInputs,
    #[error("The transaction has no outputs")]
    EmptyOutputs,
    #[error("The transaction has too many inputs")]
    TooManyInputs,
    #[error("The transaction has too many outputs")]
    TooManyOutputs,
    #[error("The transaction is underfunded")]
    Underfunded,
    #[error("Amount arithmetic overflowed u64 msat")]
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

/// All the items that may be produced during a consensus session.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Encodable, Decodable)]
pub enum ConsensusItem {
    /// A client-submitted transaction
    Tx(Transaction),
    /// Any data that modules require consensus on
    Module(wire::ModuleConsensusItem),
    /// The submitting node's bitcoin block count, trailing the chain tip by
    /// the confirmation finality delay.
    BlockCount(u32),
    /// Highest consensus version the submitting node's binary can run.
    Version(ConsensusVersion),
}
