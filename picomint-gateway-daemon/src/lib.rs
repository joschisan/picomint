pub mod cli;
pub mod connect;
pub mod db;
pub mod public;

use std::sync::Arc;

use anyhow::{anyhow, bail, ensure};
use bitcoin::Network;
use bitcoin::hashes::Hash;
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
use picomint_core::lightning::gateway::{GatewayInfo, PaymentFee};
use picomint_core::lightning::methods::{ReceiveRequest, SendRequest};
use picomint_core::secp256k1::schnorr::Signature;
use picomint_gateway_cli_core::MintInfo;
use picomint_redb::{Database, DbRead, WriteTx};

use crate::db::{OutgoingContractRow, OutgoingContractTable, Payment, PaymentTable};
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

    /// Orchestrates an outgoing payment. Registers the contract under its
    /// outpoint in the daemon-global outgoing_contract table, logs
    /// `SendEvent` on the source mint, and either starts the payment or
    /// cancels it: once the contract is known to be ours and funded, every
    /// reason not to pay is answered with the forfeit signature, since an
    /// error would leave the funding locked with no way out. Returns once a
    /// terminal event (`SendSuccessEvent` / `SendCancelEvent`) is observed
    /// in the source mint's event log.
    pub async fn send(
        &self,
        payload: SendRequest,
    ) -> anyhow::Result<std::result::Result<[u8; 32], Signature>> {
        // The forfeit signature comes from the claim key, so a contract keyed
        // to another gateway is the one request nothing here can settle.
        ensure!(
            payload.contract.claim_pk == self.client.gateway_pk(payload.mint)?,
            "The outgoing contract is keyed to another gateway"
        );

        let api = self.client.api(payload.mint)?;

        let contract_id = api::await_outgoing_contract(&api, payload.outpoint)
            .await
            .map_err(|_| anyhow!("The gateway cannot reach the mint"))?;

        ensure!(
            contract_id == payload.contract.contract_id(),
            "Contract Id returned by the mint does not match contract in request"
        );

        let operation = OperationId::from_encodable(&payload.outpoint);

        let dbtx = self.gateway_db.begin_write();

        if dbtx
            .insert(
                &OutgoingContractTable,
                &operation,
                &OutgoingContractRow {
                    mint: payload.mint,
                    contract: payload.contract.clone(),
                    outpoint: payload.outpoint,
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

        self.client.gateway_log_send_started(
            payload.mint,
            &dbtx,
            operation,
            payload.outpoint,
            payload.contract.amount,
            payload.contract.fee,
        )?;

        if let Err(error) = self.start_payment(&dbtx, &payload, operation) {
            warn!(%error, %operation, "Cancelling the payment");

            self.client.gateway_finalize_send(
                payload.mint,
                &dbtx,
                operation,
                payload.contract,
                payload.outpoint,
                None,
            )?;
        }

        dbtx.commit();

        self.client
            .gateway_subscribe_send(payload.mint, operation)
            .await
    }

    /// Checks the request against the funded contract and either kicks off
    /// an LN send via LDK or settles a direct swap: funds the contract on
    /// the target mint and claims the send. Either advances the invoice's
    /// payment hash in [`PaymentTable`]: an external invoice takes a hash
    /// nothing uses yet, an invoice of this gateway takes a registered
    /// contract it funds. Any other state is refused and the funding
    /// refunded, which is safe because the forfeit signature releases only
    /// the funding it names.
    fn start_payment(
        &self,
        dbtx: &WriteTx,
        payload: &SendRequest,
        operation: OperationId,
    ) -> anyhow::Result<()> {
        ensure!(
            payload.contract.verify_invoice(&payload.invoice),
            "The invoice is not the one the contract commits to"
        );

        let amount = payload
            .invoice
            .amount_milli_satoshis()
            .ok_or(anyhow!("Invoice is missing amount"))?;

        ensure!(
            *payload.invoice.payment_hash() == payload.contract.payment_hash,
            "The invoice's payment hash does not match the contract's payment hash"
        );

        // The invoice's expiry is deliberately not checked here. Attempting
        // the payment fails it via the LDK `PaymentFailed` event, which hands
        // the sender the forfeit signature just the same, and neither
        // ldk-node nor LDK enforce the expiry either, so an invoice the payee
        // still honors is simply paid.

        ensure!(
            payload.contract.amount == Amount(amount),
            "Contract amount does not match invoice amount"
        );

        let fee = self.send_fee.fee(amount);

        // The client priced the contract from a probe, so the fee may have
        // changed since.
        ensure!(
            payload.contract.fee == fee,
            "Contract fee does not match the send fee"
        );

        let payment = dbtx.get(&PaymentTable, &payload.contract.payment_hash);

        if self.node.node_id() != payload.invoice.get_payee_pub_key() {
            ensure!(
                payment.is_none(),
                "The invoice already has a payment attempt"
            );

            // The whole fee is the routing budget: whatever routing does not
            // take is the gateway's margin, and an internal settlement keeps
            // all of it.
            let rpc = RouteParametersConfig::default()
                .with_max_total_routing_fee_msat(fee.0)
                .with_max_total_cltv_expiry_delta(self.cltv_expiry_delta);

            // A hash LDK holds for a payment made outside the table, as one
            // the operator made through the CLI, is refused as a duplicate
            // and leaves the hash to that payment.
            self.node
                .bolt11_payment()
                .send(&payload.invoice, Some(rpc))
                .map_err(|error| anyhow!("LDK refused the outgoing payment: {error}"))?;

            dbtx.insert(
                &PaymentTable,
                &payload.contract.payment_hash,
                &Payment::Sending { operation },
            );

            return Ok(());
        }

        // An invoice of our own node that no receive registered, as one the
        // operator issued through the CLI, or one whose hash is used up.
        let Some(Payment::Registered { mint, contract }) = payment else {
            bail!("No unfunded contract is registered for this payment hash");
        };

        ensure!(contract.amount.0 == amount, "Direct-swap amount mismatch");

        let preimage = contract.preimage();

        self.client
            .gateway_start_receive(
                mint,
                dbtx,
                OperationId(payload.contract.payment_hash),
                contract,
            )
            .map_err(|error| anyhow!("Could not fund the direct swap's receive: {error}"))?;

        dbtx.insert(
            &PaymentTable,
            &payload.contract.payment_hash,
            &Payment::Done,
        );

        // The preimage is the contract's own hash, so the send is claimed
        // with the funding rather than once the mint accepts it, as an
        // inbound HTLC is. An internal settlement routes nothing, so it
        // realized no routing cost.
        self.client.gateway_finalize_send(
            payload.mint,
            dbtx,
            operation,
            payload.contract.clone(),
            payload.outpoint,
            Some((preimage, Amount::ZERO)),
        )
    }

    /// Creates a Bolt11 invoice against the payment hash of the
    /// `IncomingContract` the recipient authored, and registers the
    /// contract under that hash in [`PaymentTable`]. A hash already in use
    /// is refused before LDK sees it: LDK's `receive_for_hash` overwrites
    /// what it stores for a hash it already holds, which would reset a paid
    /// invoice to pending.
    pub async fn receive(&self, payload: ReceiveRequest) -> anyhow::Result<Bolt11Invoice> {
        ensure!(
            self.client.config(payload.mint).is_some(),
            "Mint is not added"
        );

        ensure!(
            payload.contract.fee == self.receive_fee.fee(payload.contract.amount.0),
            "Contract fee does not match the gateway receive fee"
        );

        ensure!(
            payload.contract.claim_amount().is_some(),
            "Contract fee exceeds the contract amount"
        );

        let dbtx = self.gateway_db.begin_write();

        ensure!(
            dbtx.get(&PaymentTable, &payload.contract.payment_hash())
                .is_none(),
            "The payment hash is already in use"
        );

        let invoice = self
            .node
            .bolt11_payment()
            .receive_for_hash(
                payload.contract.amount.0,
                &LdkBolt11InvoiceDescription::Direct(Description::empty()),
                self.invoice_expiry_secs,
                PaymentHash(payload.contract.payment_hash().to_byte_array()),
            )
            .map_err(|e| anyhow!("Failed to create LDK invoice: {e}"))?;

        dbtx.insert(
            &PaymentTable,
            &payload.contract.payment_hash(),
            &Payment::Registered {
                mint: payload.mint,
                contract: payload.contract,
            },
        );

        dbtx.commit();

        Ok(invoice)
    }
}
