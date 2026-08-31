//! The shared server context and wire dispatch for the fixed module set.
//!
//! `Server` is plain data — config, database, bitcoin backend. The modules
//! are functions over it; dispatch match-dispatches on the wire enum variant
//! directly — no trait indirection.

use std::time::Instant;

use picomint_bitcoin_rpc::BitcoinRpcMonitor;
use picomint_core::module::audit::AuditSummary;
use picomint_core::secp256k1::XOnlyPublicKey;
use picomint_core::tx::{Transaction, TxError};
use picomint_core::wire;
use picomint_core::{Amount, OutPoint, PeerId};
use picomint_sqlite::{Database, WriteTx};
use tracing::info;

use crate::config::ServerConfig;
use crate::consensus::tx::FundingVerifier;
use crate::consensus::{ln, mint, wallet};

#[derive(Clone)]
pub struct Server {
    pub cfg: ServerConfig,
    pub db: Database,
    pub btc_rpc: BitcoinRpcMonitor,
}

impl Server {
    pub async fn process_module_ci(
        &self,
        dbtx: &WriteTx,
        peer: PeerId,
        item: &wire::ModuleConsensusItem,
    ) -> anyhow::Result<()> {
        match item {
            wire::ModuleConsensusItem::Mint(ci) => match *ci {},
            wire::ModuleConsensusItem::Wallet(ci) => {
                wallet::process_consensus_item(self, dbtx, peer, ci.clone()).await
            }
            wire::ModuleConsensusItem::Ln(ci) => {
                ln::process_consensus_item(self, dbtx, peer, ci.clone())
            }
        }
    }

    pub fn process_input(
        &self,
        dbtx: &WriteTx,
        input: &wire::Input,
    ) -> Result<(Amount, XOnlyPublicKey), wire::InputError> {
        match input {
            wire::Input::Mint(i) => {
                mint::process_input(self, dbtx, i).map_err(wire::InputError::Mint)
            }
            wire::Input::Wallet(i) => {
                wallet::process_input(self, dbtx, i).map_err(wire::InputError::Wallet)
            }
            wire::Input::Ln(i) => ln::process_input(self, dbtx, i).map_err(wire::InputError::Ln),
        }
    }

    pub fn process_output(
        &self,
        dbtx: &WriteTx,
        output: &wire::Output,
        out_point: OutPoint,
    ) -> Result<Amount, wire::OutputError> {
        match output {
            wire::Output::Mint(o) => {
                mint::process_output(self, dbtx, o, out_point).map_err(wire::OutputError::Mint)
            }
            wire::Output::Wallet(o) => {
                wallet::process_output(self, dbtx, o, out_point).map_err(wire::OutputError::Wallet)
            }
            wire::Output::Ln(o) => {
                ln::process_output(self, dbtx, o, out_point).map_err(wire::OutputError::Ln)
            }
        }
    }

    fn input_fee(&self, input: &wire::Input) -> Amount {
        match input {
            wire::Input::Mint(..) => self.cfg.consensus.mint.input_fee,
            wire::Input::Wallet(..) => self.cfg.consensus.wallet.input_fee,
            wire::Input::Ln(..) => self.cfg.consensus.ln.input_fee,
        }
    }

    fn output_fee(&self, output: &wire::Output) -> Amount {
        match output {
            wire::Output::Mint(..) => self.cfg.consensus.mint.output_fee,
            wire::Output::Wallet(..) => self.cfg.consensus.wallet.output_fee,
            wire::Output::Ln(..) => self.cfg.consensus.ln.output_fee,
        }
    }

    pub fn audit(&self, dbtx: &WriteTx) -> AuditSummary {
        AuditSummary::new(mint::audit(dbtx), wallet::audit(dbtx), ln::audit(dbtx))
    }

    /// Dispatch the inputs and outputs of a transaction to the relevant
    /// modules.
    pub fn process_tx(&self, dbtx: &WriteTx, tx: &Transaction) -> Result<(), TxError> {
        if tx.inputs.is_empty() {
            return Err(TxError::EmptyInputs);
        }

        if tx.outputs.is_empty() {
            return Err(TxError::EmptyOutputs);
        }

        if tx.inputs.len() > Transaction::MAX_INPUTS {
            return Err(TxError::TooManyInputs);
        }

        if tx.outputs.len() > Transaction::MAX_OUTPUTS {
            return Err(TxError::TooManyOutputs);
        }

        // Ahead of the inputs rather than alongside them: the count is what a
        // signature list has to have, and knowing it is wrong costs nothing
        // next to spending every input first and finding out afterwards.
        if tx.signatures.len() != tx.inputs.len() {
            return Err(TxError::InvalidWitnessLength);
        }

        let start = Instant::now();

        let mut funding_verifier = FundingVerifier::default();
        let mut public_keys = Vec::new();

        let txid = tx.compute_txid();

        for input in &tx.inputs {
            let (amount, pub_key) = self.process_input(dbtx, input).map_err(TxError::Input)?;

            funding_verifier.add_input(amount, self.input_fee(input))?;
            public_keys.push(pub_key);
        }

        tx.validate_signatures(&public_keys)?;

        for (output, out_idx) in tx.outputs.iter().zip(0u16..) {
            let amount = self
                .process_output(dbtx, output, OutPoint { txid, out_idx })
                .map_err(TxError::Output)?;

            funding_verifier.add_output(amount, self.output_fee(output))?;
        }

        funding_verifier.verify_funding()?;

        info!(
            %txid,
            inputs = tx.inputs.len(),
            outputs = tx.outputs.len(),
            elapsed_us = start.elapsed().as_micros() as u64,
            "Verified tx",
        );

        Ok(())
    }
}
