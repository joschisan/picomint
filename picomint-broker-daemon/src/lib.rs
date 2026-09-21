pub mod cli;
pub mod db;
pub mod public;

use std::sync::Arc;

use anyhow::{Context as _, ensure};
use bitcoin::Network;
use iroh::Endpoint;
use picomint_broker_cli_core::{MintInfo, RebalanceError, RebalanceResponse};
use picomint_client::gateway::ROUTING_ACCOUNT;
use picomint_client::swap::api;
use picomint_client::{Client, Mnemonic};
use picomint_core::Amount;
use picomint_core::config::MintId;
use picomint_core::core::OperationId;
use picomint_core::lightning::gateway::PaymentFee;
use picomint_core::swap::MINIMUM_RECEIVE_CONTRACT_AMOUNT;
use picomint_core::swap::broker::BrokerInfo;
use picomint_core::swap::methods::SwapRequest;
use picomint_redb::{Database, DbRead};
use tbs::Signature;
use tracing::info;

use crate::db::SwapTable;

/// Name of the broker's database.
pub const DB_FILE: &str = "database.redb";

#[derive(Clone)]
pub struct AppState {
    pub client: Arc<Client>,
    pub endpoint: Endpoint,
    pub mnemonic: Mnemonic,
    pub db: Database,
    pub data_dir: std::path::PathBuf,
    pub network: Network,
    pub fee: PaymentFee,
    pub analytics: picomint_analytics::Analytics,
}

impl AppState {
    /// List every mint the broker has added, with its config-declared name.
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

    pub fn broker_info(&self, mint: MintId) -> anyhow::Result<BrokerInfo> {
        Ok(BrokerInfo {
            claim_pk: self.client.swap_broker_pk(mint)?,
            fee: self.fee,
        })
    }

    /// Take on a swap: verify the request against the source mint, hand it
    /// to the client, which funds the receive contract in the destination,
    /// gathers the attestation and claims the send contract with it, and
    /// answer with the attestation once the claim is submitted.
    ///
    /// Idempotent on the operation, derived from the receive contract's id
    /// as the sender derives it: a sender that lost the response asks again,
    /// finds the swap marked and gets its outcome without a second funding
    /// and without the checks, whose lookup of the send contract would
    /// otherwise wait on a contract the broker has already claimed.
    pub async fn swap(&self, req: SwapRequest) -> anyhow::Result<Signature> {
        let operation = OperationId::from_encodable(&req.receive.id());

        if self.db.begin_read().get(&SwapTable, &operation).is_some() {
            return self.outcome(operation).await;
        }

        let send = api::send_contract(&self.client.api(req.mint)?, req.outpoint)
            .await
            .map_err(|_| anyhow::anyhow!("The broker cannot reach the source mint"))?;

        ensure!(
            send.receive == req.receive.id(),
            "The send contract names another receive contract"
        );

        ensure!(
            send.claim_pk == self.client.swap_broker_pk(req.mint)?,
            "The send contract is keyed to another broker"
        );

        let destination = self
            .client
            .swap_broker_destination(send.agg_pk)
            .context("The broker does not serve the destination mint")?;

        ensure!(
            req.receive.amount >= MINIMUM_RECEIVE_CONTRACT_AMOUNT,
            "The receive amount is below the minimum"
        );

        ensure!(
            send.amount == self.fee.add_to(req.receive.amount.0),
            "The send contract does not pay the broker's fee"
        );

        let dbtx = self.db.begin_write();

        // Checked again under the write lock: a concurrent request for the
        // same swap may have marked it since the read above.
        if dbtx.get(&SwapTable, &operation).is_none() {
            dbtx.insert(&SwapTable, &operation, &());

            self.client
                .swap_broker_swap(destination, &dbtx, operation, req, send)?;

            info!(%operation, "Funding the receive contract");
        }

        dbtx.commit();

        self.outcome(operation).await
    }

    async fn outcome(&self, operation: OperationId) -> anyhow::Result<Signature> {
        self.client
            .swap_broker_subscribe(operation)
            .await
            .context("The broker could not fund the receive contract")
    }

    /// Move funds from the highest balance to the lowest onchain, by the
    /// smaller of the source's surplus and the destination's deficit
    /// against the mean, so one of the two lands on the mean, if the
    /// transfer pays for itself. The greedy for minimum cash flow: at most
    /// one transfer fewer than there are mints brings every balance to the
    /// mean. Stateless: it reads nothing but the balances, so a transfer
    /// still waiting for its confirmations is invisible to it and a run
    /// within that hour may send again. The operator runs it on a timer no
    /// tighter than a couple of hours, or by hand.
    pub async fn rebalance(&self) -> Result<Option<RebalanceResponse>, RebalanceError> {
        let balances: Vec<(MintId, Amount)> = self
            .client
            .mints()
            .into_iter()
            .map(|mint| (mint, self.client.ecash_balance(mint, ROUTING_ACCOUNT)))
            .collect();

        if balances.len() < 2 {
            return Err(RebalanceError::TooFewMints);
        }

        let (source, highest) = *balances
            .iter()
            .max_by_key(|entry| entry.1)
            .expect("at least two mints");

        let (destination, lowest) = *balances
            .iter()
            .min_by_key(|entry| entry.1)
            .expect("at least two mints");

        let total = balances.iter().map(|entry| entry.1.0).sum::<u64>();

        let mean = Amount(total / balances.len() as u64);

        let amount = bitcoin::Amount::from_sat((highest - mean).min(mean - lowest).0 / 1000);

        let send_fee = self.client.onchain_send_fee(source).await?;

        let receive_fee = self.client.onchain_receive_fee(destination).await?;

        let budget = self.fee.fee(Amount::from_sat(amount.to_sat()).0);

        if Amount::from_sat((send_fee + receive_fee).to_sat()) > budget {
            return Ok(None);
        }

        let address = self.client.onchain_receive(destination, ROUTING_ACCOUNT)?;

        let operation = self
            .client
            .onchain_send(
                source,
                ROUTING_ACCOUNT,
                address.as_unchecked().clone(),
                amount,
                None,
            )
            .await?;

        info!(%source, %destination, %amount, %operation, "Rebalancing");

        Ok(Some(RebalanceResponse {
            source,
            destination,
            amount,
            operation,
        }))
    }
}
