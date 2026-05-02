//! Concrete `Server` container for the fixed module set.
//!
//! Holds typed instances of the three canonical modules and match-dispatches
//! on the wire enum variant directly — no trait indirection.

use std::sync::Arc;

use picomint_core::module::InputMeta;
use picomint_core::module::audit::AuditSummary;
use picomint_core::tx::{Transaction, TxError};
use picomint_core::wire;
use picomint_core::{InPoint, OutPoint, PeerId};
use picomint_redb::{WriteTx, WriteTxRef};

use crate::consensus::ln::Lightning;
use crate::consensus::mint::Mint;
use crate::consensus::tx::FundingVerifier;
use crate::consensus::wallet::Wallet;

#[derive(Clone)]
pub struct Server {
    pub mint: Arc<Mint>,
    pub wallet: Arc<Wallet>,
    pub ln: Arc<Lightning>,
}

impl Server {
    pub async fn process_consensus_item(
        &self,
        dbtx: &WriteTxRef<'_>,
        item: &wire::ModuleConsensusItem,
        peer_id: PeerId,
    ) -> anyhow::Result<()> {
        match item {
            wire::ModuleConsensusItem::Mint(ci) => match *ci {},
            wire::ModuleConsensusItem::Wallet(ci) => {
                self.wallet
                    .process_consensus_item(dbtx, ci.clone(), peer_id)
                    .await
            }
            wire::ModuleConsensusItem::Ln(ci) => {
                self.ln
                    .process_consensus_item(dbtx, ci.clone(), peer_id)
                    .await
            }
        }
    }

    pub async fn process_input(
        &self,
        dbtx: &WriteTxRef<'_>,
        input: &wire::Input,
        in_point: InPoint,
    ) -> Result<InputMeta, wire::InputError> {
        match input {
            wire::Input::Mint(i) => self
                .mint
                .process_input(dbtx, i, in_point)
                .await
                .map_err(wire::InputError::Mint),
            wire::Input::Wallet(i) => self
                .wallet
                .process_input(dbtx, i, in_point)
                .await
                .map_err(wire::InputError::Wallet),
            wire::Input::Ln(i) => self
                .ln
                .process_input(dbtx, i, in_point)
                .await
                .map_err(wire::InputError::Ln),
        }
    }

    pub async fn process_output(
        &self,
        dbtx: &WriteTxRef<'_>,
        output: &wire::Output,
        out_point: OutPoint,
    ) -> Result<picomint_core::module::TxItemAmounts, wire::OutputError> {
        match output {
            wire::Output::Mint(o) => self
                .mint
                .process_output(dbtx, o, out_point)
                .await
                .map_err(wire::OutputError::Mint),
            wire::Output::Wallet(o) => self
                .wallet
                .process_output(dbtx, o, out_point)
                .await
                .map_err(wire::OutputError::Wallet),
            wire::Output::Ln(o) => self
                .ln
                .process_output(dbtx, o, out_point)
                .await
                .map_err(wire::OutputError::Ln),
        }
    }

    pub async fn audit(&self, dbtx: &WriteTx) -> AuditSummary {
        let dbtx = dbtx.as_ref();
        let mint = self.mint.audit(&dbtx).await;
        let wallet = self.wallet.audit(&dbtx).await;
        let ln = self.ln.audit(&dbtx).await;
        AuditSummary::new(mint, wallet, ln)
    }
}

/// Dispatch the inputs and outputs of a transaction to the relevant modules.
pub async fn process_tx_with_server(
    server: &Server,
    dbtx: &WriteTx,
    tx: &Transaction,
) -> Result<(), TxError> {
    if tx.inputs.is_empty() {
        return Err(TxError::EmptyInputs);
    }

    if tx.outputs.is_empty() {
        return Err(TxError::EmptyOutputs);
    }

    let mut funding_verifier = FundingVerifier::default();
    let mut public_keys = Vec::new();

    let txid = tx.compute_txid();

    for (input, in_idx) in tx.inputs.iter().zip(0u64..) {
        let meta = server
            .process_input(&dbtx.as_ref(), input, InPoint { txid, in_idx })
            .await
            .map_err(TxError::Input)?;

        funding_verifier.add_input(meta.amount)?;
        public_keys.push(meta.pub_key);
    }

    tx.validate_signatures(&public_keys)?;

    for (output, out_idx) in tx.outputs.iter().zip(0u64..) {
        let amount = server
            .process_output(&dbtx.as_ref(), output, OutPoint { txid, out_idx })
            .await
            .map_err(TxError::Output)?;

        funding_verifier.add_output(amount)?;
    }

    funding_verifier.verify_funding()?;

    Ok(())
}
