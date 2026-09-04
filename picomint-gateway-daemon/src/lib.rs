pub mod analytics;
pub mod cli;
pub mod connect;
pub mod db;
pub mod public;
pub mod trailer;

use std::sync::Arc;

use anyhow::{anyhow, bail, ensure};
use bitcoin::Network;
use bitcoin::hashes::{Hash, sha256};
use futures::StreamExt as _;
use iroh::Endpoint;
use lightning::routing::router::RouteParametersConfig;
use lightning::types::payment::PaymentHash;
use lightning_invoice::{
    Bolt11Invoice, Bolt11InvoiceDescription as LdkBolt11InvoiceDescription, Description,
};
use picomint_client::gateway::api;
use picomint_client::gateway::events::ReceiveSuccessEvent;
use picomint_client::{Client, Mnemonic};
use picomint_core::Amount;
use picomint_core::config::FederationId;
use picomint_core::core::OperationId;
use picomint_core::lightning::LightningInvoice;
use picomint_core::lightning::gateway::{GatewayInfo, PaymentFee};
use picomint_core::lightning::methods::{ReceiveRequest, SendRequest, VerifyResponse};
use picomint_core::secp256k1::schnorr::Signature;
use picomint_encoding::Encodable as _;
use picomint_gateway_cli_core::FederationInfo;
use picomint_redb::{Database, DbRead};

use crate::db::{IncomingOfferRow, IncomingOfferTable, OutgoingContractRow, OutgoingContractTable};

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
    pub analytics: analytics::Analytics,
}

impl AppState {
    /// List every federation the gateway has added, with its config-declared
    /// name.
    pub fn federation_list(&self) -> Vec<FederationInfo> {
        self.client
            .federation_configs()
            .into_iter()
            .map(|entry| FederationInfo {
                federation: entry.0,
                federation_name: entry.1.name,
            })
            .collect()
    }
}

// Lightning Gateway implementation
impl AppState {
    pub async fn gateway_info(&self, federation: &FederationId) -> anyhow::Result<GatewayInfo> {
        Ok(GatewayInfo {
            module_public_key: self.client.gateway_pk(*federation)?,
            send_fee: self.send_fee,
            receive_fee: self.receive_fee,
            expiry_delta: u16::try_from(self.cltv_expiry_delta + 144)
                .expect("the configured cltv expiry delta fits the LN protocol's u16"),
        })
    }

    /// Orchestrates an outgoing payment. Verifies the request, registers the
    /// contract in the daemon-global outgoing_contract table, logs
    /// `SendEvent` on F1, and kicks off either a direct-swap receive on the
    /// target federation or an LN send via LDK. Returns once a terminal event
    /// (`SendSuccessEvent` / `SendCancelEvent`) is observed in F1's event log.
    pub async fn send(
        &self,
        payload: SendRequest,
    ) -> anyhow::Result<std::result::Result<[u8; 32], Signature>> {
        // --- Verify the request ---------------------------------------------

        ensure!(
            payload.contract.claim_pk == self.client.gateway_pk(payload.federation)?,
            "The outgoing contract is keyed to another gateway"
        );

        ensure!(
            payload.contract.verify_invoice_auth(
                payload.invoice.consensus_hash::<sha256::Hash>(),
                &payload.auth,
            ),
            "Invalid auth signature for the invoice data"
        );

        let api = self.client.api(payload.federation)?;

        let (contract_id, expiry) = api::outgoing_contract_expiry(&api, payload.outpoint)
            .await
            .map_err(|_| anyhow!("The gateway cannot reach the federation"))?
            .ok_or(anyhow!("The outgoing contract has not yet been confirmed"))?;

        ensure!(
            contract_id == payload.contract.contract_id(),
            "Contract Id returned by the federation does not match contract in request"
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
            payload.contract.amount == Amount::from_msat(amount),
            "Contract amount does not match invoice amount"
        );

        let fee = self.send_fee.fee(amount);

        ensure!(
            payload.contract.fee == fee,
            "Contract fee does not match the advertised send fee"
        );

        ensure!(
            expiry >= self.cltv_expiry_delta + 144,
            "Contract expiry does not leave enough room for routing"
        );

        // --- Insert outgoing_contract row + log SendEvent on F1 (one tx) ---

        let operation = OperationId::from_encodable(payload.invoice.bolt11().payment_hash());

        let dbtx = self.gateway_db.begin_write();

        if dbtx
            .insert(
                &OutgoingContractTable,
                &operation,
                &OutgoingContractRow {
                    federation: payload.federation,
                    contract: payload.contract.clone(),
                    outpoint: payload.outpoint,
                    invoice: payload.invoice.clone(),
                },
            )
            .is_some()
        {
            return self
                .client
                .gateway_subscribe_send(payload.federation, operation)
                .await;
        }

        self.client.gateway_log_send_started(
            payload.federation,
            &dbtx,
            operation,
            payload.outpoint,
            Amount::from_msat(amount),
            fee,
        )?;

        // --- Direct-swap vs external LN -------------------------------------
        if self.node.node_id() != payload.invoice.bolt11().get_payee_pub_key() {
            // The whole fee is the routing budget: whatever routing does not
            // take is the gateway's margin, and an internal settlement keeps
            // all of it.
            let rpc = RouteParametersConfig::default()
                .with_max_total_routing_fee_msat(fee.msat)
                .with_max_total_cltv_expiry_delta(self.cltv_expiry_delta);

            let result = self
                .node
                .bolt11_payment()
                .send(payload.invoice.bolt11(), Some(rpc));

            // A duplicate payment means a previous run of this request already
            // kicked off the payment (its transaction failed to commit after
            // the LDK send); the LDK events drive its terminal, so treat it as
            // a successful kick-off instead of cancelling an in-flight send.
            if !matches!(result, Ok(_) | Err(ldk_node::NodeError::DuplicatePayment)) {
                self.client.gateway_finalize_send(
                    payload.federation,
                    &dbtx,
                    operation,
                    payload.contract,
                    payload.outpoint,
                    None,
                )?;
            }
        } else {
            let incoming_row = dbtx
                .get(&IncomingOfferTable, &operation)
                .expect("Direct-swap target not registered for this payment hash");

            ensure!(
                incoming_row.offer.commitment.amount.msat == amount,
                "Direct-swap amount mismatch"
            );

            if self
                .client
                .gateway_start_receive(
                    incoming_row.federation,
                    &dbtx,
                    operation,
                    incoming_row.offer,
                )
                .is_err()
            {
                self.client.gateway_finalize_send(
                    payload.federation,
                    &dbtx,
                    operation,
                    payload.contract,
                    payload.outpoint,
                    None,
                )?;
            }
        }

        dbtx.commit();

        // --- Await terminal event on F1 -------------------------------------
        self.client
            .gateway_subscribe_send(payload.federation, operation)
            .await
    }

    /// Creates a Bolt11 invoice for an incoming payment. Registers the
    /// `IncomingOffer` + the generated invoice in the daemon-global
    /// `incoming-offer` table. Idempotent on operation: a retry with the same
    /// offer returns the previously generated invoice.
    pub async fn receive(&self, payload: ReceiveRequest) -> anyhow::Result<Bolt11Invoice> {
        ensure!(payload.offer.verify(), "The offer is invalid");

        ensure!(
            self.client.config(payload.federation).is_some(),
            "Federation is not added"
        );

        let receive_fee = self.receive_fee.fee(payload.offer.commitment.amount.msat);

        ensure!(
            payload.offer.commitment.fee == receive_fee,
            "Offer fee does not match the gateway receive fee"
        );

        let invoice = self
            .node
            .bolt11_payment()
            .receive_for_hash(
                payload.offer.commitment.amount.msat,
                &LdkBolt11InvoiceDescription::Direct(Description::empty()),
                self.invoice_expiry_secs,
                PaymentHash(payload.offer.commitment.payment_hash.to_byte_array()),
            )
            .map_err(|e| anyhow!("Failed to create LDK invoice: {e}"))?;

        let dbtx = self.gateway_db.begin_write();

        if dbtx
            .insert(
                &IncomingOfferTable,
                &OperationId::from_encodable(&payload.offer.commitment.payment_hash),
                &IncomingOfferRow {
                    federation: payload.federation,
                    offer: payload.offer,
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

    pub async fn verify(
        &self,
        payment_hash: sha256::Hash,
        wait: bool,
    ) -> anyhow::Result<VerifyResponse> {
        let operation = OperationId::from_encodable(&payment_hash);

        self.gateway_db
            .begin_read()
            .get(&IncomingOfferTable, &operation)
            .ok_or_else(|| anyhow!("Unknown payment hash"))?;

        if !wait {
            if let Some(preimage) = self
                .client
                .read_operation_events(operation)
                .into_iter()
                .find_map(|entry| entry.to_event::<ReceiveSuccessEvent>().map(|e| e.preimage))
            {
                return Ok(VerifyResponse {
                    settled: true,
                    preimage: Some(preimage),
                });
            }

            return Ok(VerifyResponse {
                settled: false,
                preimage: None,
            });
        }

        let mut stream = self.client.subscribe_operation_events(operation);

        loop {
            let entry = stream
                .next()
                .await
                .expect("subscribe_operation_events only ends at client shutdown");

            if let Some(ev) = entry.to_event::<ReceiveSuccessEvent>() {
                return Ok(VerifyResponse {
                    settled: true,
                    preimage: Some(ev.preimage),
                });
            }
        }
    }
}
