pub mod analytics;
pub mod cli;
pub mod client;
pub mod db;
pub mod public;
pub mod trailer;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, anyhow, bail, ensure};
use bitcoin::Network;
use bitcoin::hashes::{Hash, sha256};
use client::GatewayClientFactory;
use futures::StreamExt as _;
use lightning::routing::router::RouteParametersConfig;
use lightning::types::payment::PaymentHash;
use lightning_invoice::{
    Bolt11Invoice, Bolt11InvoiceDescription as LdkBolt11InvoiceDescription, Description,
};
use picomint_client::Client;
use picomint_client::gw::EXPIRATION_DELTA_MINIMUM;
use picomint_client::gw::events::ReceiveSuccessEvent;
use picomint_core::Amount;
use picomint_core::config::FederationId;
use picomint_core::core::OperationId;
use picomint_core::ln::LightningInvoice;
use picomint_core::ln::contracts::PaymentImage;
use picomint_core::ln::gateway_api::{
    CreateInvoiceRequest, GatewayInfo, PaymentFee, SendPaymentRequest, VerifyResponse,
};
use picomint_core::secp256k1::schnorr::Signature;
use picomint_encoding::Encodable as _;
use picomint_eventlog::EventLogger;
use picomint_gateway_cli_core::FederationInfo;
use picomint_redb::Database;
use std::sync::RwLock;

use crate::db::{
    ClientConfigTable, DisabledFederationTable, IncomingContractRow, IncomingContractTable,
    OutgoingContractRow, OutgoingContractTable,
};

/// Default Bitcoin network for testing purposes.
pub const DEFAULT_NETWORK: Network = Network::Regtest;

/// Name of the gateway's database.
pub const DB_FILE: &str = "database.redb";

/// Name of the folder for LDK node data.
pub const LDK_NODE_DB_FOLDER: &str = "ldk_node";

#[derive(Clone)]
pub struct AppState {
    pub clients: Arc<RwLock<BTreeMap<FederationId, Arc<Client>>>>,
    pub node: Arc<ldk_node::Node>,
    pub client_factory: GatewayClientFactory,
    pub gateway_db: Database,
    pub logger: EventLogger,
    pub data_dir: std::path::PathBuf,
    pub network: Network,
    pub send_fee: PaymentFee,
    pub receive_fee: PaymentFee,
    pub ln_fee: PaymentFee,
    pub analytics: analytics::Analytics,
}

impl AppState {
    /// Get a client for `federation`, lazily loading it from
    /// [`ClientConfigTable`] on cache miss. Returns `None` only if no config
    /// is persisted for that federation — i.e. the gateway has never joined
    /// it.
    ///
    /// Double-checked: read lock → cache hit returns immediately; cache miss
    /// drops the read lock, takes the write lock, re-checks the cache (in
    /// case another caller raced and inserted), and otherwise loads + inserts
    /// exactly once. The write lock is held across the load, so cold loads
    /// for *different* feds are serialized — fine because cold loads are
    /// rare and `Client::new_gateway` is fast.
    pub fn select_client(&self, federation: FederationId) -> Option<Arc<Client>> {
        if let Some(client) = self
            .clients
            .read()
            .expect("clients RwLock poisoned")
            .get(&federation)
            .cloned()
        {
            return Some(client);
        }

        let mut clients = self.clients.write().expect("clients RwLock poisoned");

        if let Some(client) = clients.get(&federation).cloned() {
            return Some(client);
        }

        let client = self.client_factory.load(&federation).ok().flatten()?;

        clients.insert(federation, client.clone());

        Some(client)
    }

    /// List every federation the gateway has joined, with its config-declared
    /// name. Reads [`ClientConfigTable`] directly so dormant federations are
    /// not forced to lazy-load.
    pub fn federation_info_all(&self) -> Vec<FederationInfo> {
        self.gateway_db
            .begin_read()
            .as_ref()
            .iter(&ClientConfigTable, |r| {
                r.map(|(federation, config)| FederationInfo {
                    federation,
                    federation_name: config.name,
                })
                .collect()
            })
    }
}

// Lightning Gateway implementation
impl AppState {
    pub async fn gateway_info(&self, federation: &FederationId) -> anyhow::Result<GatewayInfo> {
        ensure!(
            self.gateway_db
                .begin_read()
                .as_ref()
                .get(&DisabledFederationTable, federation)
                .is_none(),
            "Federation is disabled",
        );

        let client = self
            .select_client(*federation)
            .context("Federation not connected")?;

        Ok(GatewayInfo {
            lightning_public_key: self.node.node_id(),
            module_public_key: client.gw().keypair.x_only_public_key().0,
            send_fee: self.send_fee,
            receive_fee: self.receive_fee,
            ln_fee: self.ln_fee,
            expiration_delta: 1440,
        })
    }

    /// Orchestrates an outgoing payment. Verifies the request, registers the
    /// contract in the daemon-global outgoing_contract table, logs
    /// `SendEvent` on F1, and kicks off either a direct-swap receive on the
    /// target federation or an LN send via LDK. Returns once a terminal event
    /// (`SendSuccessEvent` / `SendCancelEvent`) is observed in F1's event log.
    pub async fn send_payment(
        &self,
        payload: SendPaymentRequest,
    ) -> anyhow::Result<std::result::Result<[u8; 32], Signature>> {
        let f1_client = self
            .select_client(payload.federation)
            .context("Federation not connected")?;

        // --- Verify the request ---------------------------------------------

        ensure!(
            payload.contract.claim_pk == f1_client.gw().keypair.x_only_public_key().0,
            "The outgoing contract is keyed to another gateway"
        );

        ensure!(
            payload.contract.verify_invoice_auth(
                payload.invoice.consensus_hash::<sha256::Hash>(),
                &payload.auth,
            ),
            "Invalid auth signature for the invoice data"
        );

        let (contract_id, expiration) = f1_client
            .api()
            .gw_outgoing_contract_expiration(payload.outpoint)
            .await
            .map_err(|_| anyhow!("The gateway cannot reach the federation"))?
            .ok_or(anyhow!("The outgoing contract has not yet been confirmed"))?;

        ensure!(
            contract_id == payload.contract.contract_id(),
            "Contract Id returned by the federation does not match contract in request"
        );

        let LightningInvoice::Bolt11(invoice) = &payload.invoice;
        let amount = invoice
            .amount_milli_satoshis()
            .ok_or(anyhow!("Invoice is missing amount"))?;

        ensure!(
            PaymentImage::Hash(*invoice.payment_hash()) == payload.contract.payment_image,
            "The invoice's payment hash does not match the contract's payment hash"
        );

        let operation = OperationId::from_encodable(invoice.payment_hash());

        let is_direct_swap = self.node.node_id() == invoice.get_payee_pub_key();

        let fee = self.send_fee.fee(amount);

        let ln_fee = if is_direct_swap {
            Amount::ZERO
        } else {
            self.ln_fee.fee(amount)
        };

        ensure!(
            payload.contract.amount == Amount::from_msats(amount),
            "Contract amount does not match invoice amount"
        );

        ensure!(
            payload.contract.fee == fee + ln_fee,
            "Contract fee does not match send fee + ln fee"
        );

        // --- Insert outgoing_contract row + log SendEvent on F1 (one tx) ---

        let dbtx = self.gateway_db.begin_write();

        if dbtx
            .as_ref()
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
            return Ok(f1_client.gw().subscribe_send(operation).await);
        }

        f1_client.gw().log_send_started(
            &dbtx.as_ref(),
            operation,
            payload.outpoint,
            Amount::from_msats(amount),
            ln_fee,
            fee,
        );

        // --- Direct-swap vs external LN -------------------------------------
        if is_direct_swap {
            let incoming_row = dbtx
                .as_ref()
                .get(&IncomingContractTable, &operation)
                .expect("Direct-swap target not registered for this payment hash");

            ensure!(
                incoming_row.contract.commitment.amount.msats == amount,
                "Direct-swap amount mismatch"
            );

            let f2_client = self
                .select_client(incoming_row.federation)
                .expect("Direct-swap target federation not connected");

            if f2_client
                .gw()
                .start_receive(&dbtx.as_ref(), operation, incoming_row.contract)
                .is_err()
            {
                f1_client.gw().finalize_send(
                    &dbtx.as_ref(),
                    operation,
                    payload.contract,
                    payload.outpoint,
                    None,
                    picomint_core::Amount::ZERO, // Direct swap — no routing cost
                );
            }
        } else {
            // External LN send: `ln_fee` becomes LDK's hard cap on route cost.
            let max_delay = expiration.saturating_sub(EXPIRATION_DELTA_MINIMUM);

            if self
                .node
                .bolt11_payment()
                .send(
                    invoice,
                    Some(RouteParametersConfig {
                        max_total_routing_fee_msat: Some(ln_fee.msats),
                        max_total_cltv_expiry_delta: max_delay as u32,
                        ..RouteParametersConfig::default()
                    }),
                )
                .is_err()
            {
                f1_client.gw().finalize_send(
                    &dbtx.as_ref(),
                    operation,
                    payload.contract,
                    payload.outpoint,
                    None,
                    picomint_core::Amount::ZERO, // Direct swap — no routing cost
                );
            }
        }

        dbtx.commit();

        // --- Await terminal event on F1 -------------------------------------
        Ok(f1_client.gw().subscribe_send(operation).await)
    }

    /// Creates a Bolt11 invoice for an incoming payment. Registers the
    /// `IncomingContract` + the generated invoice in the daemon-global
    /// `incoming_contract` table. Idempotent on operation: a retry with the same
    /// contract returns the previously generated invoice.
    pub async fn create_bolt11_invoice(
        &self,
        payload: CreateInvoiceRequest,
    ) -> anyhow::Result<Bolt11Invoice> {
        if !payload.contract.verify() {
            bail!("The contract is invalid");
        }

        let gateway_info = self
            .gateway_info(&payload.federation)
            .await
            .map_err(|_| anyhow!("Federation {} does not exist", payload.federation))?;

        if payload.contract.commitment.refund_pk != gateway_info.module_public_key {
            bail!("The incoming contract is keyed to another gateway");
        }

        let receive_fee = gateway_info.receive_fee.fee(payload.amount.msats);

        if payload
            .amount
            .checked_sub(receive_fee)
            .unwrap_or(Amount::ZERO)
            == Amount::ZERO
        {
            bail!("Zero amount incoming contracts are not supported");
        }

        if payload.contract.commitment.amount != payload.amount {
            bail!("Contract amount does not match the invoice amount");
        }

        if payload.contract.commitment.fee != receive_fee {
            bail!("Contract fee does not match the gateway receive fee");
        }

        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("System time before Unix epoch")
            .as_secs();

        if payload.contract.commitment.expiration <= now_secs {
            bail!("The contract has already expired");
        }

        let payment_hash = match payload.contract.commitment.payment_image {
            PaymentImage::Hash(h) => h,
            PaymentImage::Point(..) => {
                bail!("PaymentImage is not a payment hash")
            }
        };

        let operation = OperationId::from_encodable(&payment_hash);

        // Idempotency: if we already registered this contract, return its invoice.
        if let Some(existing) = self
            .gateway_db
            .begin_read()
            .as_ref()
            .get(&IncomingContractTable, &operation)
        {
            if existing.federation != payload.federation {
                bail!("PaymentHash is already registered on a different federation");
            }
            let LightningInvoice::Bolt11(existing_invoice) = existing.invoice;
            return Ok(existing_invoice);
        }

        // Description is intentionally empty: clients can't influence what
        // the gateway puts in BOLT11 invoices it signs.
        let ldk_description = LdkBolt11InvoiceDescription::Direct(Description::empty());

        let invoice = self
            .node
            .bolt11_payment()
            .receive_for_hash(
                payload.amount.msats,
                &ldk_description,
                payload.expiry_secs,
                PaymentHash(*payment_hash.as_byte_array()),
            )
            .map_err(|e| anyhow!("Failed to create LDK invoice: {e}"))?;

        let dbtx = self.gateway_db.begin_write();

        dbtx.as_ref().insert(
            &IncomingContractTable,
            &operation,
            &IncomingContractRow {
                federation: payload.federation,
                contract: payload.contract,
                invoice: LightningInvoice::Bolt11(invoice.clone()),
                amount: payload.amount,
            },
        );

        dbtx.commit();

        Ok(invoice)
    }

    pub async fn verify_bolt11_preimage(
        &self,
        payment_hash: sha256::Hash,
        wait: bool,
    ) -> anyhow::Result<VerifyResponse> {
        let operation = OperationId::from_encodable(&payment_hash);

        let row = self
            .gateway_db
            .begin_read()
            .as_ref()
            .get(&IncomingContractTable, &operation)
            .ok_or_else(|| anyhow!("Unknown payment hash"))?;

        let client = self
            .select_client(row.federation)
            .expect("source federation for incoming contract is connected");

        if !wait {
            if let Some(preimage) = client
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

        let mut stream = client.subscribe_operation_events(operation);

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
