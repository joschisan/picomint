pub mod cli;
pub mod connect;
pub mod db;
pub mod public;
pub mod trailer;

use std::sync::Arc;

use anyhow::{anyhow, bail, ensure};
use bitcoin::Network;
use bitcoin::hashes::{Hash, sha256};
use iroh::Endpoint;
use lightning::routing::router::RouteParametersConfig;
use lightning::types::payment::PaymentHash;
use lightning_invoice::{
    Bolt11Invoice, Bolt11InvoiceDescription as LdkBolt11InvoiceDescription, Description,
};
use picomint_client::gateway::api;
use picomint_client::{Client, Mnemonic};
use picomint_core::Amount;
use picomint_core::config::MintId;
use picomint_core::core::OperationId;
use picomint_core::lightning::LightningInvoice;
use picomint_core::lightning::gateway::{GatewayInfo, PaymentFee};
use picomint_core::lightning::methods::{ReceiveRequest, SendRequest};
use picomint_core::secp256k1::schnorr::Signature;
use picomint_encoding::Encodable as _;
use picomint_gateway_cli_core::MintInfo;
use picomint_redb::{Database, DbRead};

use crate::db::{
    IncomingContractRow, IncomingContractTable, OutgoingContractRow, OutgoingContractTable,
};
use tracing::warn;

/// Name of the gateway's database.
pub const DB_FILE: &str = "database.redb";

/// Name of the folder for LDK node data.
pub const LDK_NODE_DB_FOLDER: &str = "ldk_node";

#[derive(Clone)]
pub struct AppState {
    pub client: Arc<Client>,
    pub endpoint: Endpoint,
    pub mnemonic: Mnemonic,
    pub node: Arc<ldk_node::Node>,
    pub gateway_db: Database,
    pub data_dir: std::path::PathBuf,
    pub network: Network,
    pub send_fee: PaymentFee,
    pub receive_fee: PaymentFee,
    pub invoice_expiry_secs: u32,
    pub cltv_expiry_delta: u32,
    pub analytics: picomint_analytics::Analytics,
}

impl AppState {
    /// List every mint the gateway has added, with its config-declared
    /// name.
    pub fn mint_list(&self) -> Vec<MintInfo> {
        self.client
            .mint_configs()
            .into_iter()
            .map(|entry| MintInfo {
                mint: entry.0,
                mint_name: entry.1.name,
            })
            .collect()
    }
}

// Lightning Gateway implementation
impl AppState {
    pub async fn gateway_info(&self, mint: &MintId) -> anyhow::Result<GatewayInfo> {
        Ok(GatewayInfo {
            module_public_key: self.client.gateway_pk(*mint)?,
            send_fee: self.send_fee,
            receive_fee: self.receive_fee,
        })
    }

    /// Orchestrates an outgoing payment. Verifies the request, registers the
    /// contract in the daemon-global outgoing_contract table, logs
    /// `SendEvent` on the source mint, and kicks off either a direct-swap receive on the
    /// target mint or an LN send via LDK. Returns once a terminal event
    /// (`SendSuccessEvent` / `SendCancelEvent`) is observed in the source mint's event log.
    pub async fn send(
        &self,
        payload: SendRequest,
    ) -> anyhow::Result<std::result::Result<[u8; 32], Signature>> {
        // --- Verify the request ---------------------------------------------

        ensure!(
            payload.contract.claim_pk == self.client.gateway_pk(payload.mint)?,
            "The outgoing contract is keyed to another gateway"
        );

        ensure!(
            payload.contract.verify_invoice_auth(
                payload.invoice.consensus_hash::<sha256::Hash>(),
                &payload.auth,
            ),
            "Invalid auth signature for the invoice data"
        );

        let api = self.client.api(payload.mint)?;

        let contract_id = api::await_outgoing_contract(&api, payload.outpoint)
            .await
            .map_err(|_| anyhow!("The gateway cannot reach the mint"))?;

        ensure!(
            contract_id == payload.contract.contract_id(),
            "Contract Id returned by the mint does not match contract in request"
        );

        let amount = payload
            .invoice
            .bolt11()
            .amount_milli_satoshis()
            .ok_or(anyhow!("Invoice is missing amount"))?;

        ensure!(
            *payload.invoice.bolt11().payment_hash() == payload.contract.payment_hash,
            "The invoice's payment hash does not match the contract's payment hash"
        );

        // The invoice's expiry is deliberately not checked here. Rejecting the
        // request returns a plain error, which leaves the sender's contract
        // funded until it times out, while attempting the payment fails it via
        // the LDK `PaymentFailed` event and hence hands the sender a forfeit
        // signature to reclaim the funds immediately. Neither ldk-node nor LDK
        // enforce the expiry either, so an invoice the payee still honors is
        // simply paid.

        ensure!(
            payload.contract.amount == Amount(amount),
            "Contract amount does not match invoice amount"
        );

        // --- Insert outgoing_contract row + log SendEvent on the source mint (one tx) ---

        let operation = OperationId::from_encodable(payload.invoice.bolt11().payment_hash());

        let dbtx = self.gateway_db.begin_write();

        if dbtx
            .insert(
                &OutgoingContractTable,
                &operation,
                &OutgoingContractRow {
                    mint: payload.mint,
                    contract: payload.contract.clone(),
                    outpoint: payload.outpoint,
                    invoice: payload.invoice.clone(),
                },
            )
            .is_some()
        {
            // The terminal event awaited below is written by the LDK
            // event loop through this same database, so the write
            // transaction has to be gone before the wait starts.
            drop(dbtx);

            return self
                .client
                .gateway_subscribe_send(payload.mint, operation)
                .await;
        }

        let fee = self.send_fee.fee(amount);

        self.client.gateway_log_send_started(
            payload.mint,
            &dbtx,
            operation,
            payload.outpoint,
            Amount(amount),
            fee,
        )?;

        // The client priced the contract from a probe, so a fee changed since
        // is answered with the forfeit signature, not an error: an error
        // would leave the contract funded with no way out.
        if payload.contract.fee != fee {
            warn!(%operation, "Contract fee does not match the send fee; cancelling the payment");

            self.client.gateway_finalize_send(
                payload.mint,
                &dbtx,
                operation,
                payload.contract,
                payload.outpoint,
                None,
            )?;

            dbtx.commit();

            return self
                .client
                .gateway_subscribe_send(payload.mint, operation)
                .await;
        }

        // --- Direct-swap vs external LN -------------------------------------
        if self.node.node_id() != payload.invoice.bolt11().get_payee_pub_key() {
            // The whole fee is the routing budget: whatever routing does not
            // take is the gateway's margin, and an internal settlement keeps
            // all of it.
            let rpc = RouteParametersConfig::default()
                .with_max_total_routing_fee_msat(fee.0)
                .with_max_total_cltv_expiry_delta(self.cltv_expiry_delta);

            let result = self
                .node
                .bolt11_payment()
                .send(payload.invoice.bolt11(), Some(rpc));

            // A duplicate payment means a previous run of this request already
            // kicked off the payment (its transaction failed to commit after
            // the LDK send); the LDK events drive its terminal, so treat it as
            // a successful kick-off instead of cancelling an in-flight send.
            if let Err(error) = &result
                && !matches!(error, ldk_node::NodeError::DuplicatePayment)
            {
                warn!(%error, %operation, "LDK refused the outgoing payment; cancelling it");

                self.client.gateway_finalize_send(
                    payload.mint,
                    &dbtx,
                    operation,
                    payload.contract,
                    payload.outpoint,
                    None,
                )?;
            }
        } else {
            let incoming_row = dbtx
                .get(&IncomingContractTable, &operation)
                .expect("Direct-swap target not registered for this payment hash");

            ensure!(
                incoming_row.contract.amount.0 == amount,
                "Direct-swap amount mismatch"
            );

            if let Err(error) = self.client.gateway_start_receive(
                incoming_row.mint,
                &dbtx,
                operation,
                incoming_row.contract,
            ) {
                warn!(%error, %operation, "Could not fund the direct swap's receive; cancelling the send");

                self.client.gateway_finalize_send(
                    payload.mint,
                    &dbtx,
                    operation,
                    payload.contract,
                    payload.outpoint,
                    None,
                )?;
            }
        }

        dbtx.commit();

        // --- Await terminal event on the source mint -------------------------------------
        self.client
            .gateway_subscribe_send(payload.mint, operation)
            .await
    }

    /// Creates a Bolt11 invoice against the payment hash of the
    /// `IncomingContract` the recipient authored, and registers the
    /// contract with the invoice in the daemon-global `incoming-contract`
    /// table. A repeated contract is rejected: the table insert and LDK's
    /// `receive_for_hash` both refuse a payment hash they hold.
    pub async fn receive(&self, payload: ReceiveRequest) -> anyhow::Result<Bolt11Invoice> {
        ensure!(
            self.client.config(payload.mint).is_some(),
            "Mint is not added"
        );

        ensure!(
            payload.contract.fee == self.receive_fee.fee(payload.contract.amount.0),
            "Contract fee does not match the gateway receive fee"
        );

        let contract = payload.contract;

        let invoice = self
            .node
            .bolt11_payment()
            .receive_for_hash(
                contract.amount.0,
                &LdkBolt11InvoiceDescription::Direct(Description::empty()),
                self.invoice_expiry_secs,
                PaymentHash(contract.payment_hash().to_byte_array()),
            )
            .map_err(|e| anyhow!("Failed to create LDK invoice: {e}"))?;

        let dbtx = self.gateway_db.begin_write();

        if dbtx
            .insert(
                &IncomingContractTable,
                &OperationId::from_encodable(&contract.payment_hash()),
                &IncomingContractRow {
                    mint: payload.mint,
                    contract,
                    invoice: LightningInvoice::Bolt11(invoice.clone()),
                },
            )
            .is_some()
        {
            bail!("A contract for this hash has already been registered")
        }

        dbtx.commit();

        Ok(invoice)
    }
}
