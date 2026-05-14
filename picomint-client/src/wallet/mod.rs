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
use picomint_core::core::OperationId;
use picomint_core::wallet::config::WalletConfigConsensus;
use picomint_core::wallet::{
    StandardScript, WalletInput, WalletOutput, descriptor, is_potential_receive,
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

#[derive(Clone)]
pub struct WalletClientContext {
    pub client_ctx: ClientContext,
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
    ) -> anyhow::Result<WalletClientModule> {
        let federation = context.federation();
        let sm_context = WalletClientContext {
            client_ctx: context.clone(),
        };
        let send_executor = ModuleExecutor::new(
            context.db().clone(),
            SendStateMachineTable(federation),
            sm_context,
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

        Ok(module)
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

    /// Send an onchain payment with the given fee.
    pub async fn send(
        &self,
        address: Address<NetworkUnchecked>,
        amount: bitcoin::Amount,
        fee: Option<bitcoin::Amount>,
    ) -> Result<OperationId, SendError> {
        if !address.is_valid_for_network(self.client_ctx.network()) {
            return Err(SendError::WrongNetwork);
        }

        if amount < self.cfg.dust_limit {
            return Err(SendError::DustValue);
        }

        let fee = match fee {
            Some(fee) => fee,
            None => self
                .client_ctx
                .api()
                .wallet_send_fee()
                .await
                .map_err(|_| SendError::FederationError)?
                .ok_or(SendError::NoConsensusFeerateAvailable)?,
        };

        let operation = OperationId::new_random();

        let destination = StandardScript::from_address(&address.clone().assume_checked())
            .ok_or(SendError::UnsupportedAddress)?;

        let tx_builder = TxBuilder::from_output(Output {
            output: wire::Output::Wallet(WalletOutput {
                destination,
                value: amount,
                fee,
            }),
            amount: Amount::from_sats((amount + fee).to_sat()),
            fee: self.cfg.output_fee,
        });

        let dbtx = self.client_ctx.db().begin_write();

        let txid = self
            .mint
            .finalize_and_submit_tx(&dbtx, operation, tx_builder, |txid| SendEvent {
                txid,
                address,
                amount,
                fee,
            })
            .map_err(|_| SendError::InsufficientFunds)?;

        let sm = SendStateMachine {
            operation,
            outpoint: OutPoint { txid, out_idx: 0 },
            amount,
            fee,
        };

        self.send_executor.add_state_machine_dbtx(&dbtx, sm);

        dbtx.commit();

        Ok(operation)
    }

    /// Returns the next unused receive address, polling until the initial
    /// address derivation has completed.
    pub async fn receive(&self) -> Address {
        loop {
            let idx = self
                .client_ctx
                .db()
                .begin_read()
                .iter(&ValidAddressIndexTable(self.client_ctx.federation()), |r| {
                    r.next_back().map(|(k, ())| k)
                });

            if let Some(idx) = idx {
                return self.derive_address(idx);
            }

            sleep(Duration::from_secs(1)).await;
        }
    }

    fn derive_address(&self, index: u64) -> Address {
        descriptor(
            &self.cfg.bitcoin_pks,
            &self
                .derive_tweak(index)
                .x_only_public_key()
                .0
                .consensus_hash(),
        )
        .address(self.client_ctx.network())
    }

    fn derive_tweak(&self, index: u64) -> Keypair {
        self.secret.address_keypair(index)
    }

    /// Find the next valid index starting from (and including) `start_index`.
    #[allow(clippy::maybe_infinite_iter)]
    fn next_valid_index(&self, start_index: u64) -> u64 {
        let pks_hash = self.cfg.bitcoin_pks.consensus_hash();

        block_in_place(|| {
            (start_index..)
                .find(|i| is_potential_receive(&pks_hash, &self.derive_address(*i).script_pubkey()))
                .expect("Will always find a valid index")
        })
    }

    /// Issue ecash for an unspent output with a given fee.
    fn receive_output(
        &self,
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
                tweak: self.derive_tweak(address_index).x_only_public_key().0,
            }),
            keypair: self.derive_tweak(address_index),
            amount: Amount::from_sats((amount - fee).to_sat()),
            fee: self.cfg.input_fee,
        });

        let dbtx = self.client_ctx.db().begin_write();

        let address = self.derive_address(address_index).as_unchecked().clone();

        let txid = self
            .mint
            .finalize_and_submit_tx(&dbtx, operation, tx_builder, |txid| ReceiveEvent {
                txid,
                address,
                amount,
                fee,
            })
            .expect("Input amount is sufficient to finalize transaction");

        dbtx.commit();

        (operation, txid)
    }

    async fn output_scanner(module: WalletClientModule) {
        let has_seed = module.client_ctx.db().begin_read().iter(
            &ValidAddressIndexTable(module.client_ctx.federation()),
            |r| r.next().is_some(),
        );

        if !has_seed {
            let index = module.next_valid_index(0);
            let dbtx = module.client_ctx.db().begin_write();
            assert!(
                dbtx.insert(
                    &ValidAddressIndexTable(module.client_ctx.federation()),
                    &index,
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
            .get(&NextOutputIndexTable(self.client_ctx.federation()), &())
            .unwrap_or(0);

        let mut valid_indices: Vec<u64> = dbtx
            .iter(&ValidAddressIndexTable(self.client_ctx.federation()), |r| {
                r.map(|(idx, ())| idx).collect()
            });

        drop(dbtx);

        let mut address_map: BTreeMap<ScriptBuf, u64> = valid_indices
            .iter()
            .map(|&i| (self.derive_address(i).script_pubkey(), i))
            .collect();

        let outputs = self
            .client_ctx
            .api()
            .wallet_output_info_slice(next_output_index, next_output_index + SLICE_SIZE)
            .await
            .map_err(|_| anyhow!("Failed to fetch wallet output info slice"))?;

        for output in &outputs {
            if let Some(&address_index) = address_map.get(&output.script) {
                let next_address_index = valid_indices
                    .last()
                    .copied()
                    .expect("we have at least one address index");

                // If we used the highest valid index, add the next valid one
                if address_index == next_address_index {
                    let index = self.next_valid_index(next_address_index + 1);

                    let dbtx = self.client_ctx.db().begin_write();

                    dbtx.insert(
                        &ValidAddressIndexTable(self.client_ctx.federation()),
                        &index,
                        &(),
                    );

                    dbtx.commit();

                    valid_indices.push(index);

                    address_map.insert(self.derive_address(index).script_pubkey(), index);
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
                &NextOutputIndexTable(self.client_ctx.federation()),
                &(),
                &(output.index + 1),
            );

            dbtx.commit();
        }

        Ok(!outputs.is_empty())
    }
}

/// Drop every redb table this module owns under the caller's prefix.
/// Called by [`crate::Client::wipe`] for end-of-life client cleanup.
pub(crate) fn wipe_tables(
    dbtx: &picomint_redb::WriteTx,
    federation: picomint_core::config::FederationId,
) {
    dbtx.delete_table(&NextOutputIndexTable(federation));
    dbtx.delete_table(&ValidAddressIndexTable(federation));
    dbtx.delete_table(&SendStateMachineTable(federation));
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
