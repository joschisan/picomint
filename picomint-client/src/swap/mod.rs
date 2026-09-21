pub mod api;
mod broker;
mod broker_sm;
mod db;
pub mod events;
mod secret;
mod send_sm;

use std::collections::BTreeMap;
use std::sync::Arc;

use iroh::PublicKey;
use picomint_core::config::MintId;
use picomint_core::core::{Account, OperationId};
use picomint_core::error::ErrorCode;
use picomint_core::secp256k1::{Keypair, SecretKey};
use picomint_core::swap::broker::{BrokerInfo, BrokerPk};
use picomint_core::swap::methods::{BrokerMethod, InfoRequest, InfoResponse};
use picomint_core::swap::{
    MINIMUM_RECEIVE_CONTRACT_AMOUNT, ReceiveContract, ReceiveContractSummary, SendContract,
    SwapAddress, SwapInput, SwapOutput,
};
use picomint_core::{Amount, OutPoint, secp256k1, wire};
use picomint_redb::{Database, DbRead, ReadTx, WriteTx};
use thiserror::Error;
use tokio::sync::Notify;

use crate::client::{Client, NotAddedError};
use crate::context::ClientContext;
use crate::pool::{Peer, Pool};
use crate::tx::{Input, Output, TxBuilder};

use self::broker_sm::{BrokerStateMachine, BrokerStateMachineTable};
use self::db::{BrokerPkTable, ReceiveContractStreamCursorTable};
use self::events::{ReceiveEvent, SendDirectEvent, SendEvent};
pub use self::secret::SwapSecret;
use self::send_sm::{SendStateMachine, SendStateMachineTable};

/// Contracts pulled per round trip when walking the receive-contract
/// stream; see the lightning module's incoming-contract scan for the
/// sizing.
const BATCH: u64 = 1000;

impl Peer for BrokerPk {
    fn iroh_pk(&self) -> PublicKey {
        self.0
    }
}

/// The mint's announced brokers, each with a kept-alive connection and its
/// latest probed info.
pub(crate) type Brokers = Pool<BrokerPk, BrokerInfo>;

/// Resume this mint's persisted send state machines and start the
/// receive-contract scan plus the cold-start broker warmup. Called exactly
/// once, at mint bring-up of a user client.
pub(crate) fn resume(ctx: &ClientContext) {
    crate::executor::resume::<SendStateMachine, _>(ctx, SendStateMachineTable);

    ctx.tg.spawn(receive_scan(ctx.clone()));

    ctx.tg.spawn(update_broker_pks(ctx.clone()));

    ctx.tg.spawn(update_broker_info(ctx.clone()));
}

/// Resume this mint's persisted broker state machines. Called exactly
/// once, at mint bring-up of a broker client, which neither scans for
/// receive contracts nor pools brokers — that would include dialing its
/// own key.
pub(crate) fn resume_broker(ctx: &ClientContext) {
    crate::executor::resume::<BrokerStateMachine, _>(ctx, BrokerStateMachineTable);
}

/// Fetch the mint's announced broker pk list via threshold consensus,
/// persist it to [`BrokerPkTable`] (replacing the previous set), and
/// reconcile the connection pool to match. Info is filled in separately by
/// [`update_broker_info`].
async fn update_broker_pks(ctx: ClientContext) -> Result<(), RefreshBrokersError> {
    let list = api::brokers(&ctx.api)
        .await
        .map_err(|_| RefreshBrokersError::FailedToRequestBrokers)?;

    let dbtx = ctx.db.begin_write();

    dbtx.remove_prefix(&BrokerPkTable, &ctx.mint);

    for broker in &list {
        dbtx.insert(&BrokerPkTable, &(ctx.mint, *broker), &());
    }

    dbtx.commit();

    ctx.brokers.reconcile(&list, true);

    Ok(())
}

/// Probe every broker in [`BrokerPkTable`] and refresh its info, add-only
/// so it can run concurrently with [`update_broker_pks`].
async fn update_broker_info(ctx: ClientContext) {
    let list: Vec<BrokerPk> = ctx.db.begin_read().prefix(&BrokerPkTable, &ctx.mint, |it| {
        it.map(|entry| entry.0.1).collect()
    });

    ctx.brokers.reconcile(&list, false);

    ctx.brokers
        .probe(
            &list,
            BrokerMethod::Info(InfoRequest { mint: ctx.mint }),
            |response: InfoResponse| response.info,
        )
        .await;
}

/// Try to claim a streamed receive contract: rebuild it from `sk` and, if
/// it is ours, submit the claim input and log the `ReceiveEvent` in the
/// caller's dbtx, which also advances the scanner's stream index
/// atomically.
fn receive_contract(
    ctx: &ClientContext,
    dbtx: &WriteTx,
    account: Account,
    sk: SecretKey,
    summary: &ReceiveContractSummary,
) {
    let Some(claim_keypair) = summary.recover(&sk) else {
        return;
    };

    let tx_builder = TxBuilder::from_input(Input {
        input: wire::Input::Swap(SwapInput::ClaimReceive(summary.outpoint)),
        keypair: claim_keypair,
        amount: summary.amount,
        fee: ctx.config.swap.input_fee,
    });

    let operation = OperationId::from_encodable(&summary.id);

    let amount = summary.amount;

    crate::ecash::finalize_and_submit_tx(
        ctx,
        dbtx,
        account,
        operation,
        tx_builder,
        Vec::new(),
        false,
        |txid| ReceiveEvent { txid, amount },
    )
    .expect("Cannot claim input, additional funding needed");
}

/// Walks the mint-wide receive-contract stream once, trialling every
/// account's receive key against each entry.
async fn receive_scan(ctx: ClientContext) {
    let keys = Account::ALL.map(|account| {
        (
            account,
            ctx.secret
                .swap_secret()
                .receive_keypair(account)
                .secret_key(),
        )
    });

    loop {
        let start = ctx
            .db
            .begin_read()
            .get(&ReceiveContractStreamCursorTable, &ctx.mint)
            .unwrap_or(0);

        let (entries, next) = api::await_receive_contracts(&ctx.api, start, BATCH).await;

        let dbtx = ctx.db.begin_write();

        for summary in &entries {
            for (account, sk) in keys {
                receive_contract(&ctx, &dbtx, account, sk, summary);
            }
        }

        dbtx.insert(&ReceiveContractStreamCursorTable, &ctx.mint, &next);

        dbtx.commit();
    }
}

#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum SendDirectError {
    #[error("The address is not an address of this mint")]
    NotDirect,
    #[error("Amount is too small")]
    AmountTooSmall,
    #[error("The client's balance is insufficient")]
    InsufficientBalance,
    #[error("Mint is not added")]
    NotAdded,
}

#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum SendError {
    #[error("Amount is too small")]
    AmountTooSmall,
    #[error("Broker is not available")]
    BrokerNotAvailable,
    #[error("Broker fee exceeds the allowed limit")]
    BrokerFeeExceedsLimit,
    #[error("The client's balance is insufficient")]
    InsufficientBalance,
    #[error("Mint is not added")]
    NotAdded,
}

#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum SendMaxAmountError {
    #[error("Broker is not available")]
    BrokerNotAvailable,
    #[error("Mint is not added")]
    NotAdded,
}

#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum RefreshBrokersError {
    #[error("Failed to request brokers")]
    FailedToRequestBrokers,
    #[error("Mint is not added")]
    NotAdded,
}

/// Remove every row this module owns under the caller's mint prefix.
/// Called by [`crate::Client::begin_remove_mint`] for end-of-life cleanup.
pub(crate) fn wipe_tables(dbtx: &WriteTx, mint: MintId) {
    dbtx.remove(&ReceiveContractStreamCursorTable, &mint);
    dbtx.remove_prefix(&BrokerPkTable, &mint);
    dbtx.remove_prefix(&SendStateMachineTable, &mint);
    dbtx.remove_prefix(&BrokerStateMachineTable, &mint);
}

/// Whether any of this module's state machines for `operation` is still
/// active. Operation ids are globally unique, so no mint scoping is needed.
pub(crate) fn operation_is_active(dbtx: &ReadTx, operation: OperationId) -> bool {
    dbtx.iter(&SendStateMachineTable, |r| {
        r.any(|entry| entry.1.operation == operation)
    }) || dbtx.iter(&BrokerStateMachineTable, |r| {
        r.any(|entry| entry.1.operation == operation)
    })
}

/// Notify handles for this module's state machine tables, fired on every
/// commit that writes them.
pub(crate) fn sm_notifies(db: &Database) -> Vec<Arc<Notify>> {
    vec![
        db.notify_for_table(&SendStateMachineTable),
        db.notify_for_table(&BrokerStateMachineTable),
    ]
}

// ─── Flat mint-keyed surface ───────────────────────────────────────

impl Client {
    /// The account's swap address on `mint`: static, and valid for as long
    /// as the mint exists.
    pub fn swap_receive(
        &self,
        mint: MintId,
        account: Account,
    ) -> Result<SwapAddress, NotAddedError> {
        let ctx = self.ctx(mint)?;

        Ok(SwapAddress {
            mint,
            agg_pk: ctx.config.swap.agg_pk,
            pk: ctx
                .secret
                .swap_secret()
                .receive_keypair(account)
                .public_key(),
        })
    }

    /// Every broker the mint recommends that answered a probe, keyed by pk,
    /// with the info that prices a swap through it. The info only changes
    /// on [`swap_refresh_brokers`], so a fee previewed from this map is the
    /// fee a send between refreshes pays.
    ///
    /// [`swap_refresh_brokers`]: Client::swap_refresh_brokers
    pub fn swap_brokers(
        &self,
        mint: MintId,
    ) -> Result<BTreeMap<BrokerPk, BrokerInfo>, NotAddedError> {
        Ok(self.ctx(mint)?.brokers.list())
    }

    /// Re-run the threshold-consensus broker query and re-probe every
    /// announced broker, so [`swap_brokers`] reflects the mint's current set.
    ///
    /// [`swap_brokers`]: Client::swap_brokers
    pub async fn swap_refresh_brokers(&self, mint: MintId) -> Result<(), RefreshBrokersError> {
        let ctx = self.ctx(mint).map_err(|_| RefreshBrokersError::NotAdded)?;

        update_broker_pks(ctx.clone()).await?;

        update_broker_info(ctx).await;

        Ok(())
    }

    /// Pay `address`, an address of `mint` itself, from `account`: one
    /// receive-contract output, no broker. The recipient cannot tell it from
    /// a swap.
    pub async fn swap_send_direct(
        &self,
        mint: MintId,
        account: Account,
        address: SwapAddress,
        amount: Amount,
    ) -> Result<OperationId, SendDirectError> {
        let ctx = self.ctx(mint).map_err(|_| SendDirectError::NotAdded)?;

        send_direct(&ctx, account, address, amount, false)
    }

    /// The largest whole-sat amount a [`swap_send_max_direct`] from
    /// `account` can pay.
    ///
    /// [`swap_send_max_direct`]: Client::swap_send_max_direct
    pub fn swap_send_max_amount_direct(
        &self,
        mint: MintId,
        account: Account,
    ) -> Result<Amount, NotAddedError> {
        let ctx = self.ctx(mint)?;

        Ok(send_max_amount_direct(&ctx, account))
    }

    /// Empty `account` to `address`, an address of `mint` itself: size the
    /// max, pay.
    pub async fn swap_send_max_direct(
        &self,
        mint: MintId,
        account: Account,
        address: SwapAddress,
    ) -> Result<OperationId, SendDirectError> {
        let ctx = self.ctx(mint).map_err(|_| SendDirectError::NotAdded)?;

        let amount = send_max_amount_direct(&ctx, account);

        send_direct(&ctx, account, address, amount, true)
    }

    /// Pay `address` from `account` through a broker picked from
    /// [`swap_brokers`]: lock the receive amount plus the broker's fee to
    /// the broker, then ask it to fund the receive contract in the
    /// address's mint. That mint may be `mint` itself, which is a direct
    /// payment made expensive, but the round trip exercises the broker.
    /// The operation is derived from the receive contract's id.
    ///
    /// [`swap_brokers`]: Client::swap_brokers
    pub async fn swap_send(
        &self,
        mint: MintId,
        account: Account,
        broker: BrokerPk,
        address: SwapAddress,
        amount: Amount,
    ) -> Result<OperationId, SendError> {
        let ctx = self.ctx(mint).map_err(|_| SendError::NotAdded)?;

        let info = ctx
            .brokers
            .info(broker)
            .ok_or(SendError::BrokerNotAvailable)?;

        send(&ctx, account, broker, info, address, amount, false)
    }

    /// The largest whole-sat amount a [`swap_send_max`] from `account`
    /// through this broker can pay.
    ///
    /// [`swap_send_max`]: Client::swap_send_max
    pub fn swap_send_max_amount(
        &self,
        mint: MintId,
        account: Account,
        broker: BrokerPk,
    ) -> Result<Amount, SendMaxAmountError> {
        let ctx = self.ctx(mint).map_err(|_| SendMaxAmountError::NotAdded)?;

        let info = ctx
            .brokers
            .info(broker)
            .ok_or(SendMaxAmountError::BrokerNotAvailable)?;

        Ok(send_max_amount(&ctx, account, &info))
    }

    /// Empty `account` to `address` through a broker picked from
    /// [`swap_brokers`]: size the max, pay.
    ///
    /// [`swap_brokers`]: Client::swap_brokers
    pub async fn swap_send_max(
        &self,
        mint: MintId,
        account: Account,
        broker: BrokerPk,
        address: SwapAddress,
    ) -> Result<OperationId, SendError> {
        let ctx = self.ctx(mint).map_err(|_| SendError::NotAdded)?;

        let info = ctx
            .brokers
            .info(broker)
            .ok_or(SendError::BrokerNotAvailable)?;

        let amount = send_max_amount(&ctx, account, &info);

        send(&ctx, account, broker, info, address, amount, true)
    }
}

/// The largest whole-sat amount a max send from `account` to an address of
/// the mint itself can pay: the account's notes spent in full cover the
/// amount and the mint's transaction fee, with the sub-sat remainder
/// donated.
fn send_max_amount_direct(ctx: &ClientContext, account: Account) -> Amount {
    crate::ecash::largest_affordable_amount(ctx, account, |_| ctx.config.swap.output_fee)
}

/// As [`send_max_amount_direct`], with the broker's fee on the amount
/// covered as well.
fn send_max_amount(ctx: &ClientContext, account: Account, info: &BrokerInfo) -> Amount {
    crate::ecash::largest_affordable_amount(ctx, account, |amount| {
        info.fee.fee(amount.0) + ctx.config.swap.output_fee
    })
}

fn send_direct(
    ctx: &ClientContext,
    account: Account,
    address: SwapAddress,
    amount: Amount,
    max: bool,
) -> Result<OperationId, SendDirectError> {
    if ctx.mint != address.mint {
        return Err(SendDirectError::NotDirect);
    }

    if amount < MINIMUM_RECEIVE_CONTRACT_AMOUNT {
        return Err(SendDirectError::AmountTooSmall);
    }

    let ephemeral_keypair = Keypair::new(secp256k1::SECP256K1, &mut rand::thread_rng());

    let receive = ReceiveContract::new(&address, amount, &ephemeral_keypair);

    let operation = OperationId::from_encodable(&receive.id());

    let tx_builder = TxBuilder::from_output(Output {
        output: wire::Output::Swap(SwapOutput::Receive(receive)),
        amount,
        fee: ctx.config.swap.output_fee,
    });

    let dbtx = ctx.db.begin_write();

    crate::ecash::finalize_and_submit_tx(
        ctx,
        &dbtx,
        account,
        operation,
        tx_builder,
        Vec::new(),
        max,
        |txid| SendDirectEvent { txid, amount },
    )
    .ok_or(SendDirectError::InsufficientBalance)?;

    dbtx.commit();

    Ok(operation)
}

fn send(
    ctx: &ClientContext,
    account: Account,
    broker: BrokerPk,
    info: BrokerInfo,
    address: SwapAddress,
    amount: Amount,
    max: bool,
) -> Result<OperationId, SendError> {
    if amount < MINIMUM_RECEIVE_CONTRACT_AMOUNT {
        return Err(SendError::AmountTooSmall);
    }

    if !info.fee.is_within(&BrokerInfo::FEE_LIMIT) {
        return Err(SendError::BrokerFeeExceedsLimit);
    }

    let fee = info.fee.fee(amount.0);

    let ephemeral_keypair = Keypair::new(secp256k1::SECP256K1, &mut rand::thread_rng());

    let receive = ReceiveContract::new(&address, amount, &ephemeral_keypair);

    let send = SendContract {
        amount: amount + fee,
        receive: receive.id(),
        agg_pk: address.agg_pk,
        claim_pk: info.claim_pk,
    };

    let operation = OperationId::from_encodable(&receive.id());

    let tx_builder = TxBuilder::from_output(Output {
        output: wire::Output::Swap(SwapOutput::Send(send.clone())),
        amount: amount + fee,
        fee: ctx.config.swap.output_fee,
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
        |txid| SendEvent { txid, amount, fee },
    )
    .ok_or(SendError::InsufficientBalance)?;

    let sm = SendStateMachine {
        account,
        operation,
        outpoint: OutPoint { txid, out_idx: 0 },
        send,
        receive,
        broker,
    };

    crate::executor::add_state_machine_dbtx(ctx, SendStateMachineTable, &dbtx, sm);

    dbtx.commit();

    Ok(operation)
}
