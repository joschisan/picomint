//! The client under test: a `picomint-client-daemon` process driven through
//! `picomint-client-cli`, with every outcome read back from the analytics
//! mirror through `query` — the surface an agent gets, and nothing more.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, ensure};
use lightning_invoice::Bolt11Invoice;
use picomint_analytics::table_name;
use picomint_client::ecash::{Ecash, IssuanceSuccessEvent};
use picomint_client::eventlog::Event;
use picomint_client::{TxAcceptEvent, TxRejectEvent};
use picomint_client_cli_core::{
    ClientBalanceResponse, ClientEcashCountResponse, ClientEcashReceiveResponse,
    ClientEcashSendMaxResponse, ClientEcashSendResponse, ClientExpiryResponse,
    ClientLightningGatewayListResponse, ClientLightningInvoiceReceiveResponse,
    ClientLightningInvoiceSendResponse, ClientLightningLnurlMintResponse,
    ClientLightningLnurlReceiveResponse, ClientLightningLnurlSendDirectMaxAmountResponse,
    ClientLightningLnurlSendDirectResponse, ClientLightningLnurlSendMaxAmountResponse,
    ClientListResponse, ClientOnchainReceiveResponse, ClientOnchainSendMaxAmountResponse,
    ClientOnchainSendMaxResponse, ClientOnchainSendResponse, QueryResponse,
};
use picomint_core::Amount;
use picomint_core::config::MintId;
use picomint_core::core::OperationId;
use picomint_core::ecash::Denomination;
use picomint_core::expiry::ExpiryStatus;
use picomint_core::invite::InviteCode;
use picomint_core::lightning::gateway::GatewayPk;
use serde_json::{Map, Value};
use tokio::process::Child;
use tokio::sync::Mutex;

use crate::cli::{RunCli, client_cmd};
use crate::env::retry;

/// One client daemon joined to the test mint. Every call goes to the
/// primary account: the suite never needs a second one.
#[derive(Clone)]
pub struct TestClient {
    pub data_dir: PathBuf,
    pub mint: MintId,
    process: Arc<Mutex<Child>>,
}

fn sat(amount: bitcoin::Amount) -> String {
    format!("{} sat", amount.to_sat())
}

impl TestClient {
    pub fn new(data_dir: PathBuf, mint: MintId, process: Child) -> Self {
        Self {
            data_dir,
            mint,
            process: Arc::new(Mutex::new(process)),
        }
    }

    pub fn list(&self) -> anyhow::Result<ClientListResponse> {
        client_cmd(&self.data_dir).arg("list").run_cli()
    }

    pub fn add(&self, invite: &InviteCode) -> anyhow::Result<()> {
        client_cmd(&self.data_dir)
            .arg("add")
            .arg(picomint_base32::encode(invite))
            .run_cli::<Value>()
            .map(|_| ())
    }

    /// Restore from the daemon's own mnemonic: removing the mint wipes its
    /// notes, adding it back scans them out of the mint again.
    pub fn rejoin(&self, invite: &InviteCode) -> anyhow::Result<()> {
        client_cmd(&self.data_dir)
            .arg("remove")
            .arg(self.mint.to_string())
            .run_cli::<()>()?;

        self.add(invite)
    }

    pub async fn shutdown(&self) {
        let mut process = self.process.lock().await;

        process.kill().await.expect("client daemon was running");

        process
            .wait()
            .await
            .expect("client daemon exits once killed");
    }

    pub fn balance(&self) -> anyhow::Result<Amount> {
        client_cmd(&self.data_dir)
            .arg("balance")
            .arg(self.mint.to_string())
            .arg("primary")
            .run_cli::<ClientBalanceResponse>()
            .map(|response| response.balance_msat)
    }

    pub fn expiry(&self) -> anyhow::Result<Option<ExpiryStatus>> {
        client_cmd(&self.data_dir)
            .arg("expiry")
            .arg(self.mint.to_string())
            .run_cli::<ClientExpiryResponse>()
            .map(|response| response.expiry)
    }

    pub fn ecash_count(&self) -> anyhow::Result<BTreeMap<Denomination, u64>> {
        client_cmd(&self.data_dir)
            .arg("ecash")
            .arg("count")
            .arg(self.mint.to_string())
            .arg("primary")
            .run_cli::<ClientEcashCountResponse>()
            .map(|response| response.counts)
    }

    pub fn ecash_send(&self, amount: bitcoin::Amount) -> anyhow::Result<Ecash> {
        client_cmd(&self.data_dir)
            .arg("ecash")
            .arg("send")
            .arg(self.mint.to_string())
            .arg("primary")
            .arg(sat(amount))
            .run_cli::<ClientEcashSendResponse>()
            .map(|response| response.ecash)
    }

    pub fn ecash_send_max(&self) -> anyhow::Result<Option<Ecash>> {
        client_cmd(&self.data_dir)
            .arg("ecash")
            .arg("send-max")
            .arg(self.mint.to_string())
            .arg("primary")
            .run_cli::<ClientEcashSendMaxResponse>()
            .map(|response| response.ecash)
    }

    pub fn ecash_receive(&self, ecash: &Ecash) -> anyhow::Result<OperationId> {
        client_cmd(&self.data_dir)
            .arg("ecash")
            .arg("receive")
            .arg(self.mint.to_string())
            .arg("primary")
            .arg(picomint_base32::encode(ecash))
            .run_cli::<ClientEcashReceiveResponse>()
            .map(|response| response.operation)
    }

    pub fn onchain_receive(&self) -> anyhow::Result<bitcoin::Address> {
        client_cmd(&self.data_dir)
            .arg("onchain")
            .arg("receive")
            .arg(self.mint.to_string())
            .arg("primary")
            .run_cli::<ClientOnchainReceiveResponse>()?
            .address
            .require_network(bitcoin::Network::Regtest)
            .context("the mint runs on regtest")
    }

    pub fn onchain_send(
        &self,
        address: &bitcoin::Address,
        amount: bitcoin::Amount,
        fee: Option<bitcoin::Amount>,
    ) -> anyhow::Result<OperationId> {
        let mut cmd = client_cmd(&self.data_dir);
        cmd.arg("onchain")
            .arg("send")
            .arg(self.mint.to_string())
            .arg("primary")
            .arg(address.to_string())
            .arg(sat(amount));
        if let Some(fee) = fee {
            cmd.arg("--fee").arg(sat(fee));
        }
        cmd.run_cli::<ClientOnchainSendResponse>()
            .map(|response| response.operation)
    }

    pub fn onchain_send_max_amount(&self) -> anyhow::Result<bitcoin::Amount> {
        client_cmd(&self.data_dir)
            .arg("onchain")
            .arg("send-max-amount")
            .arg(self.mint.to_string())
            .arg("primary")
            .run_cli::<ClientOnchainSendMaxAmountResponse>()
            .map(|response| response.amount_sat)
    }

    pub fn onchain_send_max(&self, address: &bitcoin::Address) -> anyhow::Result<OperationId> {
        client_cmd(&self.data_dir)
            .arg("onchain")
            .arg("send-max")
            .arg(self.mint.to_string())
            .arg("primary")
            .arg(address.to_string())
            .run_cli::<ClientOnchainSendMaxResponse>()
            .map(|response| response.operation)
    }

    pub fn lightning_gateway_refresh(&self) -> anyhow::Result<()> {
        client_cmd(&self.data_dir)
            .arg("lightning")
            .arg("gateway")
            .arg("refresh")
            .arg(self.mint.to_string())
            .run_cli::<Value>()
            .map(|_| ())
    }

    /// The one gateway the test mint recommends.
    pub fn lightning_gateway(&self) -> anyhow::Result<GatewayPk> {
        client_cmd(&self.data_dir)
            .arg("lightning")
            .arg("gateway")
            .arg("list")
            .arg(self.mint.to_string())
            .run_cli::<ClientLightningGatewayListResponse>()?
            .gateways
            .into_keys()
            .next()
            .context("no gateway has answered a probe")
    }

    pub fn lightning_invoice_send(
        &self,
        gateway: GatewayPk,
        invoice: Bolt11Invoice,
    ) -> anyhow::Result<OperationId> {
        client_cmd(&self.data_dir)
            .arg("lightning")
            .arg("invoice")
            .arg("send")
            .arg(self.mint.to_string())
            .arg("primary")
            .arg(picomint_base32::encode(&gateway))
            .arg(invoice.to_string())
            .run_cli::<ClientLightningInvoiceSendResponse>()
            .map(|response| response.operation)
    }

    pub fn lightning_invoice_receive(
        &self,
        gateway: GatewayPk,
        amount: bitcoin::Amount,
    ) -> anyhow::Result<Bolt11Invoice> {
        client_cmd(&self.data_dir)
            .arg("lightning")
            .arg("invoice")
            .arg("receive")
            .arg(self.mint.to_string())
            .arg("primary")
            .arg(picomint_base32::encode(&gateway))
            .arg(sat(amount))
            .run_cli::<ClientLightningInvoiceReceiveResponse>()
            .map(|response| response.invoice)
    }

    pub fn lightning_lnurl_send_max_amount(&self, gateway: GatewayPk) -> anyhow::Result<Amount> {
        client_cmd(&self.data_dir)
            .arg("lightning")
            .arg("lnurl")
            .arg("send-max-amount")
            .arg(self.mint.to_string())
            .arg("primary")
            .arg(picomint_base32::encode(&gateway))
            .run_cli::<ClientLightningLnurlSendMaxAmountResponse>()
            .map(|response| response.amount_msat)
    }

    pub fn lightning_lnurl_send_direct(
        &self,
        lnurl: &str,
        amount: bitcoin::Amount,
    ) -> anyhow::Result<OperationId> {
        client_cmd(&self.data_dir)
            .arg("lightning")
            .arg("lnurl")
            .arg("send-direct")
            .arg(self.mint.to_string())
            .arg("primary")
            .arg(lnurl)
            .arg(sat(amount))
            .run_cli::<ClientLightningLnurlSendDirectResponse>()
            .map(|response| response.operation)
    }

    pub fn lightning_lnurl_send_direct_max_amount(&self) -> anyhow::Result<Amount> {
        client_cmd(&self.data_dir)
            .arg("lightning")
            .arg("lnurl")
            .arg("send-direct-max-amount")
            .arg(self.mint.to_string())
            .arg("primary")
            .run_cli::<ClientLightningLnurlSendDirectMaxAmountResponse>()
            .map(|response| response.amount_msat)
    }

    pub fn lightning_lnurl_receive(&self, lnurl_daemon: &str) -> anyhow::Result<String> {
        client_cmd(&self.data_dir)
            .arg("lightning")
            .arg("lnurl")
            .arg("receive")
            .arg(self.mint.to_string())
            .arg("primary")
            .arg(lnurl_daemon)
            .run_cli::<ClientLightningLnurlReceiveResponse>()
            .map(|response| response.lnurl)
    }

    pub fn lightning_lnurl_mint(&self, lnurl: &str) -> anyhow::Result<Option<MintId>> {
        client_cmd(&self.data_dir)
            .arg("lightning")
            .arg("lnurl")
            .arg("mint")
            .arg(lnurl)
            .run_cli::<ClientLightningLnurlMintResponse>()
            .map(|response| response.mint)
    }

    pub fn query(&self, query: &str) -> anyhow::Result<Vec<Map<String, Value>>> {
        client_cmd(&self.data_dir)
            .arg("query")
            .arg(query)
            .run_cli::<QueryResponse>()
            .map(|response| response.0)
    }

    pub fn rows<E: Event>(&self, filter: &str) -> anyhow::Result<Vec<Map<String, Value>>> {
        self.query(&format!(
            "SELECT * FROM {} WHERE {filter}",
            table_name::<E>()
        ))
    }

    /// Poll the analytics until `E` has a row matching the SQL `filter`,
    /// and return that row.
    pub async fn await_row<E: Event>(&self, filter: &str) -> anyhow::Result<Map<String, Value>> {
        retry(&format!("{} where {filter}", table_name::<E>()), || async {
            self.rows::<E>(filter)?
                .into_iter()
                .next()
                .context("not logged yet")
        })
        .await
    }

    /// Poll the analytics until `E` has been logged under `operation`, and
    /// return its row.
    pub async fn await_event<E: Event>(
        &self,
        operation: OperationId,
    ) -> anyhow::Result<Map<String, Value>> {
        self.await_row::<E>(&format!("operation = '{operation}'"))
            .await
    }

    /// Wait until a reissue is settled: `Ok` once the mint accepted the
    /// transaction *and* the notes were issued, since the balance only
    /// reflects the receive after the issuance state machine has fetched
    /// its threshold signatures; `Err` with the mint's reason on rejection.
    pub async fn await_tx_outcome(
        &self,
        operation: OperationId,
    ) -> anyhow::Result<Result<(), String>> {
        let filter = format!("operation = '{operation}'");

        retry(&format!("tx outcome of {operation}"), || async {
            if let Some(reject) = self.rows::<TxRejectEvent>(&filter)?.first() {
                let error = reject["error"].as_str().context("error column")?;

                return Ok(Err(error.to_string()));
            }

            ensure!(
                !self.rows::<TxAcceptEvent>(&filter)?.is_empty(),
                "tx not accepted yet"
            );

            ensure!(
                !self.rows::<IssuanceSuccessEvent>(&filter)?.is_empty(),
                "notes not issued yet"
            );

            Ok(Ok(()))
        })
        .await
    }
}
