pub use picomint_core::onchain as common;

mod api;
mod db;
pub mod events;
mod secret;
mod send_sm;

use std::collections::BTreeMap;
use std::time::Duration;

use crate::client::Client;
use crate::context::ClientContext;
use crate::tx::{Input, Output, TxBuilder};
use anyhow::{Context, anyhow};
use bitcoin::address::NetworkUnchecked;
use bitcoin::{Address, ScriptBuf};
use db::{NextOutputIndexTable, ValidAddressIndexTable};
use events::{ReceiveEvent, SendEvent};
use picomint_core::config::MintId;
use picomint_core::core::{Account, OperationId};
use picomint_core::onchain::{
    OnchainInput, OnchainOutput, StandardScript, is_potential_receive, tweaked_address,
};
use picomint_core::wire;
use picomint_core::{Amount, OutPoint, TransactionId};
use picomint_encoding::Encodable;
use picomint_redb::{Database, DbRead, ReadTx, WriteTx};
use std::sync::Arc;
use tokio::sync::Notify;

pub use self::secret::OnchainSecret;
use secp256k1::Keypair;
use send_sm::{SendStateMachine, SendStateMachineTable};
use thiserror::Error;
use tokio::task::block_in_place;
use tokio::time::sleep;
use tracing::warn;

/// Number of output info entries to scan per batch.
const SLICE_SIZE: u64 = 1000;

/// Resume this mint's persisted onchain state machines and start the
/// address scanner. Called exactly once, at mint bring-up.
pub(crate) fn resume(ctx: &ClientContext) {
    crate::executor::resume::<SendStateMachine, _>(ctx, SendStateMachineTable);

    ctx.tg.spawn(output_scanner(ctx.clone()));
}

/// Fetch the current fee required to send an onchain payment.
pub(crate) async fn send_fee(ctx: &ClientContext) -> Result<bitcoin::Amount, SendError> {
    api::send_fee(&ctx.api)
        .await
        .map_err(|_| SendError::MintError)?
        .ok_or(SendError::NoConsensusFeerateAvailable)
}

fn max_amount_at(ctx: &ClientContext, account: Account, fee: bitcoin::Amount) -> bitcoin::Amount {
    let amount = crate::ecash::largest_affordable_amount(ctx, account, |_| {
        Amount::from_sat(fee.to_sat()) + ctx.config.onchain.output_fee
    });

    bitcoin::Amount::from_sat(amount.msat / 1000)
}

fn submit_send(
    ctx: &ClientContext,
    account: Account,
    address: Address<NetworkUnchecked>,
    amount: bitcoin::Amount,
    fee: bitcoin::Amount,
    max: bool,
) -> Result<OperationId, SendError> {
    if !address.is_valid_for_network(ctx.config.network) {
        return Err(SendError::WrongNetwork);
    }

    if amount < ctx.config.onchain.dust_limit {
        return Err(SendError::DustValue);
    }

    let operation = OperationId::new_random();

    let destination = StandardScript::from_address(&address.clone().assume_checked())
        .ok_or(SendError::UnsupportedAddress)?;

    let tx_builder = TxBuilder::from_output(Output {
        output: wire::Output::Onchain(OnchainOutput {
            destination,
            value: amount,
            fee,
        }),
        amount: Amount::from_sat((amount + fee).to_sat()),
        fee: ctx.config.onchain.output_fee,
    });

    let dbtx = ctx.db.begin_write();

    let txid = crate::ecash::finalize_and_submit_tx(
        ctx,
        &dbtx,
        account,
        operation,
        tx_builder,
        Vec::new(),
        max,
        |txid| SendEvent {
            txid,
            address,
            amount,
            fee,
        },
    )
    .ok_or(SendError::InsufficientFunds)?;

    let sm = SendStateMachine {
        account,
        operation,
        outpoint: OutPoint { txid, out_idx: 0 },
        amount,
        fee,
    };

    crate::executor::add_state_machine_dbtx(ctx, SendStateMachineTable, &dbtx, sm);

    dbtx.commit();

    Ok(operation)
}

/// The largest valid address index `account` has reached, or `None` before
/// the scanner has seeded it. All accounts share one table, so this reads
/// the tail of the account's own key prefix rather than the table's.
fn highest_valid_index(ctx: &ClientContext, account: Account) -> Option<u64> {
    ctx.db
        .begin_read()
        .prefix_rev(&ValidAddressIndexTable, &(ctx.mint, account), |r| {
            r.next().map(|entry| entry.0.2)
        })
}

fn derive_address(ctx: &ClientContext, account: Account, index: u64) -> Address {
    tweaked_address(
        &ctx.config.onchain.agg_pk,
        &derive_tweak(ctx, account, index)
            .x_only_public_key()
            .0
            .consensus_hash(),
        ctx.config.network,
    )
}

fn derive_tweak(ctx: &ClientContext, account: Account, index: u64) -> Keypair {
    ctx.secret.onchain_secret().address_keypair(account, index)
}

/// Find `account`'s next valid index starting from (and including)
/// `start_index`.
#[allow(clippy::maybe_infinite_iter)]
fn next_valid_index(ctx: &ClientContext, account: Account, start_index: u64) -> u64 {
    let pks_hash = ctx.config.onchain.agg_pk.consensus_hash();

    block_in_place(|| {
        (start_index..)
            .find(|i| {
                is_potential_receive(&pks_hash, &derive_address(ctx, account, *i).script_pubkey())
            })
            .expect("Will always find a valid index")
    })
}

/// Issue ecash into `account` for an unspent output with a given fee.
fn receive_output(
    ctx: &ClientContext,
    account: Account,
    output_index: u64,
    amount: bitcoin::Amount,
    address_index: u64,
    fee: bitcoin::Amount,
) -> (OperationId, TransactionId) {
    let operation = OperationId::new_random();

    let tx_builder = TxBuilder::from_input(Input {
        input: wire::Input::Onchain(OnchainInput {
            output_index,
            fee,
            tweak: derive_tweak(ctx, account, address_index)
                .x_only_public_key()
                .0,
        }),
        keypair: derive_tweak(ctx, account, address_index),
        amount: Amount::from_sat((amount - fee).to_sat()),
        fee: ctx.config.onchain.input_fee,
    });

    let dbtx = ctx.db.begin_write();

    let address = derive_address(ctx, account, address_index)
        .as_unchecked()
        .clone();

    let txid = crate::ecash::finalize_and_submit_tx(
        ctx,
        &dbtx,
        account,
        operation,
        tx_builder,
        Vec::new(),
        false,
        |txid| ReceiveEvent {
            txid,
            address,
            amount,
            fee,
        },
    )
    .expect("Input amount is sufficient to finalize transaction");

    dbtx.commit();

    (operation, txid)
}

/// Walks the mint-wide output stream once, matching every account's
/// addresses in the same pass. The stream and its cursor are shared, so a
/// each extra account costs another entry in the address map rather than
/// another sweep.
async fn output_scanner(ctx: ClientContext) {
    for account in Account::ALL {
        if highest_valid_index(&ctx, account).is_some() {
            continue;
        }

        let index = next_valid_index(&ctx, account, 0);
        let dbtx = ctx.db.begin_write();
        assert!(
            dbtx.insert(&ValidAddressIndexTable, &(ctx.mint, account, index), &())
                .is_none(),
            "seed address index already present"
        );
        dbtx.commit();
    }

    loop {
        match check_outputs(&ctx).await {
            Ok(skip_wait) => {
                if skip_wait {
                    continue;
                }
            }
            Err(e) => {
                warn!("Failed to fetch outputs: {e}");
            }
        }

        if ctx.config.network == bitcoin::Network::Regtest {
            sleep(Duration::from_secs(1)).await;
        } else {
            sleep(Duration::from_secs(60)).await;
        }
    }
}

async fn check_outputs(ctx: &ClientContext) -> anyhow::Result<bool> {
    let dbtx = ctx.db.begin_read();

    let start = dbtx.get(&NextOutputIndexTable, &ctx.mint).unwrap_or(0);

    // Every account's indices come out of one prefix scan, already tagged
    // with the account they belong to.
    let valid_indices: Vec<(Account, u64)> = dbtx.prefix(&ValidAddressIndexTable, &ctx.mint, |r| {
        r.map(|entry| (entry.0.1, entry.0.2)).collect()
    });

    drop(dbtx);

    let mut address_map: BTreeMap<ScriptBuf, (Account, u64)> = valid_indices
        .iter()
        .map(|&(account, i)| {
            (
                derive_address(ctx, account, i).script_pubkey(),
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

    let outputs = api::output_info_slice(&ctx.api, start, start + SLICE_SIZE)
        .await
        .map_err(|_| anyhow!("Failed to fetch onchain output info slice"))?;

    for output in &outputs {
        if let Some(&(account, address_index)) = address_map.get(&output.script) {
            let next_address_index = *frontier
                .get(&account)
                .expect("every account in the map has a frontier");

            // If we used this account's highest valid index, add its next
            // valid one
            if address_index == next_address_index {
                let index = next_valid_index(ctx, account, next_address_index + 1);

                let dbtx = ctx.db.begin_write();

                dbtx.insert(&ValidAddressIndexTable, &(ctx.mint, account, index), &());

                dbtx.commit();

                frontier.insert(account, index);

                address_map.insert(
                    derive_address(ctx, account, index).script_pubkey(),
                    (account, index),
                );
            }

            if !output.spent {
                // In order to not overpay on fees we choose to wait,
                // the congestion will clear up within a few blocks.
                if api::pending_tx_chain(&ctx.api)
                    .await
                    .map_err(|_| anyhow!("Failed to request onchain pending tx chain"))?
                    .len()
                    >= 3
                {
                    return Ok(false);
                }

                let receive_fee = api::receive_fee(&ctx.api)
                    .await
                    .map_err(|_| anyhow!("Failed to request onchain receive fee"))?
                    .context("No consensus feerate is available")?;

                if output.value > receive_fee {
                    let (operation, txid) = receive_output(
                        ctx,
                        account,
                        output.index,
                        output.value,
                        address_index,
                        receive_fee,
                    );

                    ctx.await_tx_accepted(operation, txid)
                        .await
                        .map_err(|e| anyhow!("Claim transaction was rejected: {e}"))?;
                }
            }
        }

        let dbtx = ctx.db.begin_write();

        dbtx.insert(&NextOutputIndexTable, &ctx.mint, &(output.index + 1));

        dbtx.commit();
    }

    Ok(!outputs.is_empty())
}

/// Remove every row this module owns under the caller's mint prefix.
/// Called by [`crate::Client::begin_remove_mint`] for end-of-life cleanup.
pub(crate) fn wipe_tables(dbtx: &WriteTx, mint: MintId) {
    dbtx.remove(&NextOutputIndexTable, &mint);
    dbtx.remove_prefix(&ValidAddressIndexTable, &mint);
    dbtx.remove_prefix(&SendStateMachineTable, &mint);
}

/// Whether any of this module's state machines for `operation` is still
/// active under `mint`.
pub(crate) fn operation_is_active(dbtx: &ReadTx, mint: MintId, operation: OperationId) -> bool {
    dbtx.prefix(&SendStateMachineTable, &mint, |r| {
        r.any(|entry| entry.1.operation == operation)
    })
}

/// Notify handles for this module's state machine tables, fired on every
/// commit that writes them.
pub(crate) fn sm_notifies(db: &Database) -> Vec<Arc<Notify>> {
    vec![db.notify_for_table(&SendStateMachineTable)]
}

#[derive(Error, Debug, Clone, Eq, PartialEq)]
pub enum SendError {
    #[error("Address is from a different network than the mint.")]
    WrongNetwork,
    #[error("The value is too small")]
    DustValue,
    #[error("Could not determine the send fee")]
    MintError,
    #[error("No consensus feerate is available at this time")]
    NoConsensusFeerateAvailable,
    #[error("The client does not have sufficient funds to send the payment")]
    InsufficientFunds,
    #[error("Unsupported address type")]
    UnsupportedAddress,
    #[error("Mint is not added")]
    NotAdded,
}

// ─── Flat mint-keyed surface ───────────────────────────────────────

impl Client {
    /// `account`'s next unused onchain deposit address. Errors while the
    /// initial address derivation has not completed yet.
    pub fn onchain_receive(&self, mint: MintId, account: Account) -> anyhow::Result<Address> {
        let ctx = self.ctx(mint)?;

        highest_valid_index(&ctx, account)
            .map(|index| derive_address(&ctx, account, index))
            .context("Deposit address derivation has not completed yet")
    }

    /// Send an onchain payment funded from `account`. `fee` defaults to the
    /// mint's current send fee.
    pub async fn onchain_send(
        &self,
        mint: MintId,
        account: Account,
        address: Address<NetworkUnchecked>,
        amount: bitcoin::Amount,
        fee: Option<bitcoin::Amount>,
    ) -> Result<OperationId, SendError> {
        let ctx = self.ctx(mint).map_err(|_| SendError::NotAdded)?;

        let fee = match fee {
            Some(fee) => fee,
            None => send_fee(&ctx).await?,
        };

        submit_send(&ctx, account, address, amount, fee, false)
    }

    /// The largest whole-sat amount a [`onchain_send_max`] from
    /// `account` can pay onchain at the current consensus feerate. A quote —
    /// the send itself re-prices at the moment it is submitted.
    pub async fn onchain_send_max_amount(
        &self,
        mint: MintId,
        account: Account,
    ) -> Result<bitcoin::Amount, SendError> {
        let ctx = self.ctx(mint).map_err(|_| SendError::NotAdded)?;

        Ok(max_amount_at(&ctx, account, send_fee(&ctx).await?))
    }

    /// Send `account`'s whole balance onchain by spending every note it
    /// holds; no change comes back.
    pub async fn onchain_send_max(
        &self,
        mint: MintId,
        account: Account,
        address: Address<NetworkUnchecked>,
    ) -> Result<OperationId, SendError> {
        let ctx = self.ctx(mint).map_err(|_| SendError::NotAdded)?;

        let fee = send_fee(&ctx).await?;

        let amount = max_amount_at(&ctx, account, fee);

        submit_send(&ctx, account, address, amount, fee, true)
    }

    /// The current fee required to send an onchain payment.
    pub async fn onchain_send_fee(&self, mint: MintId) -> Result<bitcoin::Amount, SendError> {
        let ctx = self.ctx(mint).map_err(|_| SendError::NotAdded)?;

        send_fee(&ctx).await
    }

    /// The total value of bitcoin controlled by the mint.
    pub async fn onchain_total_value(&self, mint: MintId) -> anyhow::Result<bitcoin::Amount> {
        let ctx = self.ctx(mint)?;

        api::mint_utxo(&ctx.api)
            .await
            .map(|tx_out| tx_out.map_or(bitcoin::Amount::ZERO, |tx_out| tx_out.value))
    }

    /// The current consensus feerate.
    pub async fn onchain_feerate(&self, mint: MintId) -> anyhow::Result<Option<u32>> {
        api::consensus_feerate(&self.ctx(mint)?.api).await
    }
}
