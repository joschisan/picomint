pub mod analytics;
pub mod cli;
pub mod client;
pub mod db;
pub mod public;
pub mod trailer;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context as _, anyhow, bail, ensure};
use bitcoin::Network;
use bitcoin::hashes::{Hash, sha256};
use client::GatewayClientFactory;
use futures::StreamExt as _;
use lightning::ln::channelmanager::PaymentId;
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
use picomint_core::ln::contracts::PaymentImage;
use picomint_core::ln::gateway_api::{
    CreateBolt11InvoicePayload, GatewayInfo, PaymentFee, SendPaymentPayload,
};
use picomint_core::ln::{Bolt11InvoiceDescription, LightningInvoice};
use picomint_core::secp256k1::schnorr::Signature;
use picomint_core::time::duration_since_epoch;
use picomint_encoding::Encodable as _;
use picomint_gateway_cli_core::FederationInfo;
use picomint_lnurl::VerifyResponse;
use picomint_logging::LOG_GATEWAY;
use picomint_redb::Database;
use std::sync::RwLock;
use tracing::warn;

use crate::db::{INCOMING_CONTRACT, IncomingContractRow, OUTGOING_CONTRACT, OutgoingContractRow};

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
    pub api_addr: SocketAddr,
    pub data_dir: std::path::PathBuf,
    pub network: Network,
    pub send_fee: PaymentFee,
    pub receive_fee: PaymentFee,
    pub ln_fee: PaymentFee,
    pub analytics: analytics::Analytics,
    pub task_group: picomint_core::task::TaskGroup,
}

impl AppState {
    /// Retrieves a client for a given federation. Sync — the `clients` map
    /// is only written at startup and is effectively read-only afterwards.
    pub fn select_client(&self, federation_id: FederationId) -> Option<Arc<Client>> {
        self.clients
            .read()
            .expect("clients RwLock poisoned")
            .get(&federation_id)
            .cloned()
    }

    /// After `load_clients`, spawn one analytics trailer per federation
    /// client so the SQLite mirror starts backfilling immediately.
    pub async fn spawn_analytics_trailers(&self) {
        let clients = self.clients.read().expect("clients RwLock poisoned");

        for (federation_id, client) in clients.iter() {
            analytics::spawn_trailer(
                &self.task_group,
                client.clone(),
                *federation_id,
                self.analytics.clone(),
            );
        }
    }

    /// Load all persisted federation clients on startup.
    pub async fn load_clients(&self) -> anyhow::Result<()> {
        let federations = self.client_factory.list_federations().await;

        let mut loaded = Vec::new();

        for federation_id in federations {
            match self.client_factory.load(&federation_id).await {
                Ok(Some(client)) => {
                    loaded.push((client.federation_id(), client));
                }
                Ok(None) => {
                    warn!(target: LOG_GATEWAY, %federation_id, "Client DB not initialized, skipping");
                }
                Err(err) => {
                    warn!(target: LOG_GATEWAY, %federation_id, %err, "Failed to load client");
                }
            }
        }

        let mut clients = self.clients.write().expect("clients RwLock poisoned");

        for (id, client) in loaded {
            clients.insert(id, client);
        }

        Ok(())
    }

    /// After `load_clients`, spawn one trailer per federation so receive
    /// events (`ReceiveSuccess` / `ReceiveRefund` / `ReceiveFailure`) trigger
    /// the external LN / cross-federation settle work out-of-band.
    pub async fn spawn_trailers(&self) {
        let clients = self.clients.read().expect("clients RwLock poisoned");
        for (federation_id, client) in clients.iter() {
            trailer::spawn_trailer(
                &self.task_group,
                self.clone(),
                *federation_id,
                client.clone(),
            );
        }
    }

    /// Get the name of a federation from its client config.
    pub async fn federation_name(client: &Arc<Client>) -> String {
        client.config().await.name
    }

    /// Snapshot the current clients map into an owned Vec so we can release
    /// the sync RwLock before entering async work.
    fn clients_snapshot(&self) -> Vec<(FederationId, Arc<Client>)> {
        self.clients
            .read()
            .expect("clients RwLock poisoned")
            .iter()
            .map(|(id, c)| (*id, c.clone()))
            .collect()
    }

    /// Get info for all connected federations.
    pub async fn federation_info_all(&self) -> Vec<FederationInfo> {
        let mut infos = Vec::new();
        for (federation_id, client) in self.clients_snapshot() {
            infos.push(FederationInfo {
                federation_id,
                federation_name: Self::federation_name(&client).await,
            });
        }
        infos
    }
}

// Lightning Gateway implementation
impl AppState {
    pub async fn gateway_info(&self, federation_id: &FederationId) -> anyhow::Result<GatewayInfo> {
        let client = self
            .select_client(*federation_id)
            .context("Federation not connected")?;

        Ok(GatewayInfo {
            lightning_public_key: self.node.node_id(),
            module_public_key: client.gw().keypair.public_key(),
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
        payload: SendPaymentPayload,
    ) -> anyhow::Result<std::result::Result<[u8; 32], Signature>> {
        let f1_client = self
            .select_client(payload.federation_id)
            .context("Federation not connected")?;

        // --- Verify the request ---------------------------------------------

        ensure!(
            payload.contract.claim_pk == f1_client.gw().keypair.public_key(),
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

        let operation_id = OperationId::from_encodable(invoice.payment_hash());

        let is_direct_swap = self.node.node_id() == invoice.get_payee_pub_key();

        let fee = self.send_fee.fee(amount);

        let ln_fee = if is_direct_swap {
            Amount::ZERO
        } else {
            self.ln_fee.fee(amount)
        };

        ensure!(
            payload.contract.amount == Amount::from_msats(amount + fee.msats + ln_fee.msats),
            "Contract amount does not match invoice amount + send fee + ln fee"
        );

        // --- Idempotency: if outgoing_contract row already exists, subscribe
        //     and return. subscribe_send replays event history, so a
        //     completed op resolves immediately.
        if self
            .gateway_db
            .begin_read()
            .as_ref()
            .get(&OUTGOING_CONTRACT, &operation_id)
            .is_some()
        {
            return Ok(f1_client.gw().subscribe_send(operation_id).await);
        }

        // --- Insert outgoing_contract row + log SendEvent on F1 (one tx) ---

        let tx = self.gateway_db.begin_write();

        tx.as_ref().insert(
            &OUTGOING_CONTRACT,
            &operation_id,
            &OutgoingContractRow {
                federation_id: payload.federation_id,
                contract: payload.contract.clone(),
                outpoint: payload.outpoint,
                invoice: payload.invoice.clone(),
            },
        );

        f1_client.gw().log_send_started(
            &tx.as_ref().isolate(payload.federation_id),
            operation_id,
            payload.outpoint,
            Amount::from_msats(amount),
            ln_fee,
            fee,
        );

        tx.commit();

        // --- Direct-swap vs external LN -------------------------------------
        if is_direct_swap {
            let incoming_row = self
                .gateway_db
                .begin_read()
                .as_ref()
                .get(&INCOMING_CONTRACT, &operation_id)
                .ok_or_else(|| {
                    anyhow!("Direct-swap target not registered for this payment hash")
                })?;

            ensure!(
                incoming_row.amount.msats == amount,
                "Direct-swap amount mismatch"
            );

            let f2_client = self
                .select_client(incoming_row.federation_id)
                .ok_or_else(|| anyhow!("Direct-swap target federation not connected"))?;

            let incoming_fee = incoming_row.amount - incoming_row.contract.commitment.amount;

            let tx = self.gateway_db.begin_write();
            f2_client
                .gw()
                .start_receive(
                    &tx.as_ref().isolate(incoming_row.federation_id),
                    operation_id,
                    incoming_row.contract,
                    incoming_fee,
                )
                .map_err(|e| anyhow!("Failed to start direct-swap receive: {e}"))?;
            tx.commit();
        } else {
            // External LN send: `ln_fee` becomes LDK's hard cap on route cost.
            let max_delay = expiration.saturating_sub(EXPIRATION_DELTA_MINIMUM);

            let payment_id = PaymentId(invoice.payment_hash().to_byte_array());
            if self.node.payment(&payment_id).is_none() {
                self.node
                    .bolt11_payment()
                    .send(
                        invoice,
                        Some(RouteParametersConfig {
                            max_total_routing_fee_msat: Some(ln_fee.msats),
                            max_total_cltv_expiry_delta: max_delay as u32,
                            ..RouteParametersConfig::default()
                        }),
                    )
                    .map_err(|e| anyhow!("LDK payment failed to initialize: {e:?}"))?;
            }
        }

        // --- Await terminal event on F1 -------------------------------------
        Ok(f1_client.gw().subscribe_send(operation_id).await)
    }

    /// Creates a Bolt11 invoice for an incoming payment. Registers the
    /// `IncomingContract` + the generated invoice in the daemon-global
    /// `incoming_contract` table. Idempotent on op_id: a retry with the same
    /// contract returns the previously generated invoice.
    pub async fn create_bolt11_invoice(
        &self,
        payload: CreateBolt11InvoicePayload,
    ) -> anyhow::Result<Bolt11Invoice> {
        if !payload.contract.verify() {
            bail!("The contract is invalid");
        }

        let gateway_info = self
            .gateway_info(&payload.federation_id)
            .await
            .map_err(|_| anyhow!("Federation {} does not exist", payload.federation_id))?;

        if payload.contract.commitment.refund_pk != gateway_info.module_public_key {
            bail!("The incoming contract is keyed to another gateway");
        }

        let contract_amount = gateway_info.receive_fee.subtract_from(payload.amount.msats);

        if contract_amount == Amount::ZERO {
            bail!("Zero amount incoming contracts are not supported");
        }

        if contract_amount != payload.contract.commitment.amount {
            bail!("The contract amount does not pay the correct amount of fees");
        }

        if payload.contract.commitment.expiration <= duration_since_epoch().as_secs() {
            bail!("The contract has already expired");
        }

        let payment_hash = match payload.contract.commitment.payment_image {
            PaymentImage::Hash(h) => h,
            PaymentImage::Point(..) => {
                bail!("PaymentImage is not a payment hash")
            }
        };

        let operation_id = OperationId::from_encodable(&payment_hash);

        // Idempotency: if we already registered this contract, return its invoice.
        if let Some(existing) = self
            .gateway_db
            .begin_read()
            .as_ref()
            .get(&INCOMING_CONTRACT, &operation_id)
        {
            if existing.federation_id != payload.federation_id {
                bail!("PaymentHash is already registered on a different federation");
            }
            let LightningInvoice::Bolt11(existing_invoice) = existing.invoice;
            return Ok(existing_invoice);
        }

        let ldk_description = match payload.description.clone() {
            Bolt11InvoiceDescription::Direct(desc) => LdkBolt11InvoiceDescription::Direct(
                Description::new(desc).map_err(|_| anyhow!("Invalid invoice description"))?,
            ),
            Bolt11InvoiceDescription::Hash(hash) => {
                LdkBolt11InvoiceDescription::Hash(lightning_invoice::Sha256(hash))
            }
        };

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

        let tx = self.gateway_db.begin_write();

        tx.as_ref().insert(
            &INCOMING_CONTRACT,
            &operation_id,
            &IncomingContractRow {
                federation_id: payload.federation_id,
                contract: payload.contract,
                invoice: LightningInvoice::Bolt11(invoice.clone()),
                amount: payload.amount,
            },
        );

        tx.commit();

        Ok(invoice)
    }

    pub async fn verify_bolt11_preimage(
        &self,
        payment_hash: sha256::Hash,
        wait: bool,
    ) -> anyhow::Result<VerifyResponse> {
        let operation_id = OperationId::from_encodable(&payment_hash);

        let row = self
            .gateway_db
            .begin_read()
            .as_ref()
            .get(&INCOMING_CONTRACT, &operation_id)
            .ok_or_else(|| anyhow!("Unknown payment hash"))?;

        let client = self
            .select_client(row.federation_id)
            .expect("source federation for incoming contract is connected");

        if !wait {
            if let Some(preimage) = client
                .read_operation_events(operation_id)
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

        let mut stream = client.subscribe_operation_events(operation_id);

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
