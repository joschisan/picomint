pub use picomint_core::wallet as common;

mod api;
mod db;
pub mod events;
mod secret;
mod send_sm;

use std::collections::BTreeMap;
use std::time::Duration;

use crate::executor::ModuleExecutor;
use crate::module::ClientContext;
use crate::task::TaskGroup;
use crate::tx::{Input, Output, TxBuilder};
use anyhow::{Context, anyhow};
use bitcoin::address::NetworkUnchecked;
use bitcoin::{Address, ScriptBuf};
use db::{NextOutputIndexTable, ValidAddressIndexTable};
use events::{ReceiveEvent, SendEvent};
use picomint_core::core::{Account, OperationId};
use picomint_core::wallet::config::WalletConfigConsensus;
use picomint_core::wallet::{
    StandardScript, WalletInput, WalletOutput, is_potential_receive, tweaked_address,
};
use picomint_core::wire;
use picomint_core::{Amount, OutPoint, TransactionId};
use picomint_encoding::Encodable;

pub use self::secret::WalletSecret;
use secp256k1::Keypair;
use send_sm::{SendStateMachine, SendStateMachineTable};
use thiserror::Error;
use tokio::task::block_in_place;
use tokio::time::sleep;
use tracing::warn;

/// Number of output info entries to scan per batch.
const SLICE_SIZE: u64 = 1000;

#[derive(Clone)]
pub struct WalletClientModule {
    secret: WalletSecret,
    cfg: WalletConfigConsensus,
    client_ctx: ClientContext,
    mint: std::sync::Arc<crate::mint::MintClientModule>,
    send_executor: ModuleExecutor<SendStateMachine, SendStateMachineTable>,
}

impl WalletClientModule {
    pub fn input_fee(&self) -> Amount {
        self.cfg.input_fee
    }

    pub fn output_fee(&self) -> Amount {
        self.cfg.output_fee
    }
}

impl WalletClientModule {
    pub fn new(
        cfg: WalletConfigConsensus,
        context: ClientContext,
        mint: std::sync::Arc<crate::mint::MintClientModule>,
        secret: WalletSecret,
        tg: &TaskGroup,
    ) -> WalletClientModule {
        let federation = context.federation();
        let send_executor = ModuleExecutor::new(
            context.db().clone(),
            federation,
            SendStateMachineTable,
            context.clone(),
            tg.clone(),
        );

        let module = WalletClientModule {
            secret,
            cfg,
            client_ctx: context,
            mint,
            send_executor,
        };

        tg.spawn(Self::output_scanner(module.clone()));

        module
    }
}

impl WalletClientModule {
    /// Returns the Bitcoin network for this federation.
    pub fn get_network(&self) -> bitcoin::Network {
        self.client_ctx.network()
    }

    /// Fetch the total value of bitcoin controlled by the federation.
    pub async fn total_value(&self) -> anyhow::Result<bitcoin::Amount> {
        self.client_ctx
            .api()
            .wallet_federation_wallet()
            .await
            .map(|tx_out| tx_out.map_or(bitcoin::Amount::ZERO, |tx_out| tx_out.value))
    }

    /// Fetch the consensus block count of the federation.
    pub async fn block_count(&self) -> anyhow::Result<u64> {
        self.client_ctx.api().wallet_consensus_block_count().await
    }

    /// Fetch the current consensus feerate.
    pub async fn feerate(&self) -> anyhow::Result<Option<u64>> {
        self.client_ctx.api().wallet_consensus_feerate().await
    }

    /// Fetch the current fee required to send an onchain payment.
    pub async fn send_fee(&self) -> Result<bitcoin::Amount, SendError> {
        self.client_ctx
            .api()
            .wallet_send_fee()
            .await
            .map_err(|_| SendError::FederationError)?
            .ok_or(SendError::NoConsensusFeerateAvailable)
    }

    /// Send an onchain payment with the given fee, funded from `account`.
    pub async fn send(
        &self,
        account: Account,
        address: Address<NetworkUnchecked>,
        amount: bitcoin::Amount,
        fee: Option<bitcoin::Amount>,
    ) -> Result<OperationId, SendError> {
        let fee = match fee {
            Some(fee) => fee,
            None => self.send_fee().await?,
        };

        self.submit_send(account, address, amount, fee, false)
    }

    /// The largest whole-sat amount a [`Self::send_max`] from `account` can
    /// pay onchain at the current consensus feerate: the account's notes
    /// spent in full cover the payment, its onchain fee, the federation's
    /// transaction fee and the integrator's cut, with less than a sat left
    /// over. The send itself re-prices at the moment it is submitted, so
    /// this is a quote — a feerate that moves in between moves the amount
    /// with it.
    pub async fn send_max_amount(&self, account: Account) -> Result<bitcoin::Amount, SendError> {
        Ok(self.max_amount_at(account, self.send_fee().await?))
    }

    fn max_amount_at(&self, account: Account, fee: bitcoin::Amount) -> bitcoin::Amount {
        let amount = self.mint.largest_affordable_amount(account, |_| {
            Amount::from_sat(fee.to_sat()) + self.cfg.output_fee
        });

        bitcoin::Amount::from_sat(amount.msat / 1000)
    }

    /// Send `account`'s whole balance onchain by spending every note it
    /// holds. Identical to [`Self::send`] except that the amount is
    /// [`Self::send_max_amount`]'s to compute rather than the caller's to
    /// choose, and that change is minted at the max-send floor: no change
    /// comes back and the sub-sat remainder is donated to the federation.
    pub async fn send_max(
        &self,
        account: Account,
        address: Address<NetworkUnchecked>,
    ) -> Result<OperationId, SendError> {
        let fee = self.send_fee().await?;

        let amount = self.max_amount_at(account, fee);

        self.submit_send(account, address, amount, fee, true)
    }

    fn submit_send(
        &self,
        account: Account,
        address: Address<NetworkUnchecked>,
        amount: bitcoin::Amount,
        fee: bitcoin::Amount,
        max: bool,
    ) -> Result<OperationId, SendError> {
        if !address.is_valid_for_network(self.client_ctx.network()) {
            return Err(SendError::WrongNetwork);
        }

        if amount < self.cfg.dust_limit {
            return Err(SendError::DustValue);
        }

        let operation = OperationId::new_random();

        let destination = StandardScript::from_address(&address.clone().assume_checked())
            .ok_or(SendError::UnsupportedAddress)?;

        let tx_builder = TxBuilder::from_output(Output {
            output: wire::Output::Wallet(WalletOutput {
                destination,
                value: amount,
                fee,
            }),
            amount: Amount::from_sat((amount + fee).to_sat()),
            fee: self.cfg.output_fee,
        });

        let dbtx = self.client_ctx.db().begin_write();

        let txid = self
            .mint
            .finalize_and_submit_tx(&dbtx, account, operation, tx_builder, max, |txid| {
                SendEvent {
                    txid,
                    address,
                    amount,
                    fee,
                }
            })
            .ok_or(SendError::InsufficientFunds)?;

        let sm = SendStateMachine {
            account,
            operation,
            outpoint: OutPoint { txid, out_idx: 0 },
            amount,
            fee,
        };

        self.send_executor.add_state_machine_dbtx(&dbtx, sm);

        dbtx.commit();

        Ok(operation)
    }

    /// Returns `account`'s next unused receive address, polling until the
    /// initial address derivation has completed.
    pub async fn receive(&self, account: Account) -> Address {
        loop {
            if let Some(idx) = self.highest_valid_index(account) {
                return self.derive_address(account, idx);
            }

            sleep(Duration::from_secs(1)).await;
        }
    }

    /// The largest valid address index `account` has reached, or `None` before
    /// the scanner has seeded it. All accounts share one table, so this reads
    /// the tail of the account's own key prefix rather than the table's.
    fn highest_valid_index(&self, account: Account) -> Option<u64> {
        self.client_ctx.db().begin_read().prefix_rev(
            &ValidAddressIndexTable,
            &(self.client_ctx.federation(), account),
            |r| r.next().map(|entry| entry.0.2),
        )
    }

    fn derive_address(&self, account: Account, index: u64) -> Address {
        tweaked_address(
            &self.cfg.agg_pk,
            &self
                .derive_tweak(account, index)
                .x_only_public_key()
                .0
                .consensus_hash(),
            self.client_ctx.network(),
        )
    }

    fn derive_tweak(&self, account: Account, index: u64) -> Keypair {
        self.secret.address_keypair(account, index)
    }

    /// Find `account`'s next valid index starting from (and including)
    /// `start_index`.
    #[allow(clippy::maybe_infinite_iter)]
    fn next_valid_index(&self, account: Account, start_index: u64) -> u64 {
        let pks_hash = self.cfg.agg_pk.consensus_hash();

        block_in_place(|| {
            (start_index..)
                .find(|i| {
                    is_potential_receive(
                        &pks_hash,
                        &self.derive_address(account, *i).script_pubkey(),
                    )
                })
                .expect("Will always find a valid index")
        })
    }

    /// Issue ecash into `account` for an unspent output with a given fee.
    fn receive_output(
        &self,
        account: Account,
        output_index: u64,
        amount: bitcoin::Amount,
        address_index: u64,
        fee: bitcoin::Amount,
    ) -> (OperationId, TransactionId) {
        let operation = OperationId::new_random();

        let tx_builder = TxBuilder::from_input(Input {
            input: wire::Input::Wallet(WalletInput {
                output_index,
                fee,
                tweak: self
                    .derive_tweak(account, address_index)
                    .x_only_public_key()
                    .0,
            }),
            keypair: self.derive_tweak(account, address_index),
            amount: Amount::from_sat((amount - fee).to_sat()),
            fee: self.cfg.input_fee,
        });

        let dbtx = self.client_ctx.db().begin_write();

        let address = self
            .derive_address(account, address_index)
            .as_unchecked()
            .clone();

        let txid = self
            .mint
            .finalize_and_submit_tx(&dbtx, account, operation, tx_builder, false, |txid| {
                ReceiveEvent {
                    txid,
                    address,
                    amount,
                    fee,
                }
            })
            .expect("Input amount is sufficient to finalize transaction");

        dbtx.commit();

        (operation, txid)
    }

    /// Walks the federation-wide output stream once, matching every account's
    /// addresses in the same pass. The stream and its cursor are shared, so a
    /// each extra account costs another entry in the address map rather than
    /// another sweep.
    async fn output_scanner(module: WalletClientModule) {
        for account in Account::USER_ACCOUNTS {
            if module.highest_valid_index(account).is_some() {
                continue;
            }

            let index = module.next_valid_index(account, 0);
            let dbtx = module.client_ctx.db().begin_write();
            assert!(
                dbtx.insert(
                    &ValidAddressIndexTable,
                    &(module.client_ctx.federation(), account, index),
                    &()
                )
                .is_none(),
                "seed address index already present"
            );
            dbtx.commit();
        }

        loop {
            match module.check_outputs().await {
                Ok(skip_wait) => {
                    if skip_wait {
                        continue;
                    }
                }
                Err(e) => {
                    warn!("Failed to fetch outputs: {e}");
                }
            }

            if module.client_ctx.network() == bitcoin::Network::Regtest {
                sleep(Duration::from_secs(1)).await;
            } else {
                sleep(Duration::from_secs(60)).await;
            }
        }
    }

    async fn check_outputs(&self) -> anyhow::Result<bool> {
        let dbtx = self.client_ctx.db().begin_read();

        let next_output_index = dbtx
            .get(&NextOutputIndexTable, &self.client_ctx.federation())
            .unwrap_or(0);

        // Every account's indices come out of one prefix scan, already tagged
        // with the account they belong to.
        let valid_indices: Vec<(Account, u64)> = dbtx.prefix(
            &ValidAddressIndexTable,
            &self.client_ctx.federation(),
            |r| r.map(|entry| (entry.0.1, entry.0.2)).collect(),
        );

        drop(dbtx);

        let mut address_map: BTreeMap<ScriptBuf, (Account, u64)> = valid_indices
            .iter()
            .map(|&(account, i)| {
                (
                    self.derive_address(account, i).script_pubkey(),
                    (account, i),
                )
            })
            .collect();

        // Highest index reached per account, so a match on an account's
        // frontier extends that account rather than the whole table's.
        let mut frontier: BTreeMap<Account, u64> = BTreeMap::new();
        for &(account, i) in &valid_indices {
            frontier
                .entry(account)
                .and_modify(|highest| *highest = (*highest).max(i))
                .or_insert(i);
        }

        let outputs = self
            .client_ctx
            .api()
            .wallet_output_info_slice(next_output_index, next_output_index + SLICE_SIZE)
            .await
            .map_err(|_| anyhow!("Failed to fetch wallet output info slice"))?;

        for output in &outputs {
            if let Some(&(account, address_index)) = address_map.get(&output.script) {
                let next_address_index = *frontier
                    .get(&account)
                    .expect("every account in the map has a frontier");

                // If we used this account's highest valid index, add its next
                // valid one
                if address_index == next_address_index {
                    let index = self.next_valid_index(account, next_address_index + 1);

                    let dbtx = self.client_ctx.db().begin_write();

                    dbtx.insert(
                        &ValidAddressIndexTable,
                        &(self.client_ctx.federation(), account, index),
                        &(),
                    );

                    dbtx.commit();

                    frontier.insert(account, index);

                    address_map.insert(
                        self.derive_address(account, index).script_pubkey(),
                        (account, index),
                    );
                }

                if !output.spent {
                    // In order to not overpay on fees we choose to wait,
                    // the congestion will clear up within a few blocks.
                    if self
                        .client_ctx
                        .api()
                        .wallet_pending_tx_chain()
                        .await
                        .map_err(|_| anyhow!("Failed to request wallet pending tx chain"))?
                        .len()
                        >= 3
                    {
                        return Ok(false);
                    }

                    let receive_fee = self
                        .client_ctx
                        .api()
                        .wallet_receive_fee()
                        .await
                        .map_err(|_| anyhow!("Failed to request wallet receive fee"))?
                        .context("No consensus feerate is available")?;

                    if output.value > receive_fee {
                        let (operation, txid) = self.receive_output(
                            account,
                            output.index,
                            output.value,
                            address_index,
                            receive_fee,
                        );

                        self.client_ctx
                            .await_tx_accepted(operation, txid)
                            .await
                            .map_err(|e| anyhow!("Claim transaction was rejected: {e}"))?;
                    }
                }
            }

            let dbtx = self.client_ctx.db().begin_write();

            dbtx.insert(
                &NextOutputIndexTable,
                &self.client_ctx.federation(),
                &(output.index + 1),
            );

            dbtx.commit();
        }

        Ok(!outputs.is_empty())
    }
}

/// Remove every row this module owns under the caller's federation prefix.
/// Called by [`crate::Client::wipe`] for end-of-life client cleanup.
pub(crate) fn wipe_tables(
    dbtx: &picomint_sqlite::WriteTx,
    federation: picomint_core::config::FederationId,
) {
    dbtx.remove(&NextOutputIndexTable, &federation);
    dbtx.remove_prefix(&ValidAddressIndexTable, &federation);
    dbtx.remove_prefix(&SendStateMachineTable, &federation);
}

#[derive(Error, Debug, Clone, Eq, PartialEq)]
pub enum SendError {
    #[error("Address is from a different network than the federation.")]
    WrongNetwork,
    #[error("The value is too small")]
    DustValue,
    #[error("Could not determine the send fee")]
    FederationError,
    #[error("No consensus feerate is available at this time")]
    NoConsensusFeerateAvailable,
    #[error("The client does not have sufficient funds to send the payment")]
    InsufficientFunds,
    #[error("Unsupported address type")]
    UnsupportedAddress,
}
