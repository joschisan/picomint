pub use picomint_core::lightning as common;

mod api;
mod db;
pub mod events;
mod gateway;
mod secret;
mod send_sm;

use picomint_redb::{Database, DbRead, ReadTx, WriteTx};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::Notify;

use crate::client::{Client, NotAddedError};
use crate::context::ClientContext;
use crate::tx::{Input, Output, TxBuilder};
use bitcoin::secp256k1;
use db::{GatewayPkTable, IncomingContractStreamCursorTable, SendOperationIdTable};
pub(crate) use gateway::Gateways;
use lightning_invoice::{Bolt11Invoice, Currency};
use picomint_core::NumNodesExt;
use picomint_core::config::MintId;
use picomint_core::core::{Account, OperationId};
use picomint_core::error::ErrorCode;
use picomint_core::lightning::contracts::{IncomingContract, OutgoingContract};
use picomint_core::lightning::gateway::{GatewayInfo, GatewayPk, PaymentFee};
use picomint_core::lightning::lnurl::LnurlRequest;
use picomint_core::lightning::{
    LightningInput, LightningInvoice, LightningOutput, MINIMUM_INCOMING_CONTRACT_AMOUNT,
};
use picomint_core::methods::MintInfoResponse;
use picomint_core::wire;

pub use self::secret::LightningSecret;
use picomint_core::{Amount, OutPoint};
use picomint_encoding::Encodable;
use picomint_lnurl::Lnurl;
use rand::seq::IteratorRandom;
use secp256k1::{Keypair, PublicKey, SecretKey};
use thiserror::Error;
use url::Url;

use self::events::{ReceiveEvent, SendEvent};
use self::send_sm::{SendSMCommon, SendSMState, SendStateMachine, SendStateMachineTable};

/// Contracts pulled per round trip when walking the incoming-contract
/// stream.
///
/// The stream is mint-wide and a cold client walks all of it, so the
/// batch sets how many round trips that costs — and each round trip fans out
/// to every node. A thousand summaries is ~150 kB per node, which is a
/// reasonable unit of work against a set that only grows with the
/// mint's unclaimed contracts.
const BATCH: u64 = 1000;

/// Resume this mint's persisted send state machines and start the
/// incoming-contract scan plus the cold-start gateway warmup. Called
/// exactly once, at mint bring-up.
///
/// The warmup runs concurrently: the info probe reads whatever pks the
/// previous session persisted, so `lightning_gateways` fills without
/// waiting on the threshold-consensus pk query.
pub(crate) fn resume(ctx: &ClientContext) {
    crate::executor::resume::<SendStateMachine, _>(ctx, SendStateMachineTable);

    ctx.tg.spawn(receive_scan(ctx.clone()));

    ctx.tg.spawn(update_gateway_pks(ctx.clone()));

    ctx.tg.spawn(update_gateway_info(ctx.clone()));
}

/// Fetch the mint's announced gateway pk list via threshold
/// consensus, persist it to [`GatewayPkTable`] (replacing the previous
/// set), and reconcile the connection pool to match — a deregistered
/// gateway is dropped here, its connection aborted. Info is filled in
/// separately by [`update_gateway_info`].
async fn update_gateway_pks(ctx: ClientContext) -> Result<(), RefreshGatewaysError> {
    let list = api::gateways(&ctx.api)
        .await
        .map_err(|_| RefreshGatewaysError::FailedToRequestGateways)?;

    let dbtx = ctx.db.begin_write();

    dbtx.remove_prefix(&GatewayPkTable, &ctx.mint);

    for gateway_pk in &list {
        dbtx.insert(&GatewayPkTable, &(ctx.mint, *gateway_pk), &());
    }

    dbtx.commit();

    ctx.gateways.reconcile(&list, true);

    Ok(())
}

/// Probe every gateway in [`GatewayPkTable`] and refresh its info. Ensures
/// a connection to each (add-only — never removing a gateway, so it can run
/// concurrently with [`update_gateway_pks`]) and probes over it; a
/// gateway that fails to answer is left unselectable.
async fn update_gateway_info(ctx: ClientContext) {
    let list: Vec<GatewayPk> = ctx
        .db
        .begin_read()
        .prefix(&GatewayPkTable, &ctx.mint, |it| {
            it.map(|entry| entry.0.1).collect()
        });

    ctx.gateways.reconcile(&list, false);

    ctx.gateways.probe(&list, ctx.mint).await;
}

/// The largest whole-sat amount a max send from `account` can pay: the
/// account's notes spent in full cover the amount, the gateway's fee on
/// it if it goes through one, and the mint's transaction fee, with the
/// sub-sat remainder donated.
fn send_max_amount(
    ctx: &ClientContext,
    account: Account,
    gateway_info: Option<&GatewayInfo>,
) -> Amount {
    crate::ecash::largest_affordable_amount(ctx, account, |amount| {
        gateway_info.map_or(Amount::ZERO, |info| info.send_fee.fee(amount.0))
            + ctx.config.lightning.output_fee
    })
}

/// Resolve `lnurl` to an invoice for `amount` and pay it through the
/// gateway. The invoice has to carry the amount asked for: an lnurl
/// endpoint does not get to pick what the sender pays. A `max` send
/// spends every note the account holds and leaves none; an amount the
/// endpoint refuses fails at resolution rather than sending a clamped
/// payment, and the balance stays where it is.
async fn lnurl_send(
    ctx: &ClientContext,
    account: Account,
    gateway_pk: GatewayPk,
    gateway_info: GatewayInfo,
    lnurl: &Lnurl,
    amount: Amount,
    max: bool,
) -> Result<OperationId, LnurlSendError> {
    let info = picomint_lnurl::request(lnurl.url())
        .await
        .map_err(LnurlSendError::Lnurl)?;

    let invoice = picomint_lnurl::get_invoice(&info, amount.0)
        .await
        .map_err(LnurlSendError::Lnurl)?
        .pr;

    if invoice.amount_milli_satoshis() != Some(amount.0) {
        return Err(LnurlSendError::InvoiceAmountMismatch);
    }

    Ok(send_inner(ctx, account, gateway_pk, gateway_info, invoice, max).await?)
}

/// The request behind an lnurl, if it is one an lnurl daemon serves: the
/// payload is the path segment after `pay/`.
fn decode_lnurl_request(lnurl: &Lnurl) -> Option<LnurlRequest> {
    lnurl
        .url()
        .rsplit_once("/pay/")
        .and_then(|split| picomint_base32::decode(split.1).ok())
}

/// The recipient an lnurl of this mint names.
fn lnurl_recipient(ctx: &ClientContext, lnurl: &Lnurl) -> Result<PublicKey, LnurlSendDirectError> {
    decode_lnurl_request(lnurl)
        .filter(|request| {
            request.info == MintInfoResponse::new(&ctx.config).consensus_hash_sha256()
        })
        .map(|request| request.recipient)
        .ok_or(LnurlSendDirectError::NotThisMint)
}

/// Pay the account of this mint an lnurl names straight from `account`:
/// the sender authors the incoming contract a gateway would otherwise
/// fund and funds it itself, at no fee. There is nothing to settle
/// afterwards, so the funding transaction's acceptance is the payment;
/// the recipient's scanner finds the contract as it finds any other. A
/// `max` send spends every note the account holds and leaves none.
fn lnurl_send_direct(
    ctx: &ClientContext,
    account: Account,
    lnurl: &Lnurl,
    amount: Amount,
    max: bool,
) -> Result<OperationId, LnurlSendDirectError> {
    let recipient = lnurl_recipient(ctx, lnurl)?;

    if amount < MINIMUM_INCOMING_CONTRACT_AMOUNT {
        return Err(LnurlSendDirectError::AmountTooSmall);
    }

    let contract = IncomingContract::author(&recipient, amount, Amount::ZERO);

    let operation = OperationId::from_encodable(&contract.payment_hash());

    let tx_builder = TxBuilder::from_output(Output {
        output: wire::Output::Lightning(Box::new(LightningOutput::Incoming(contract))),
        amount,
        fee: ctx.config.lightning.output_fee,
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
        |txid| SendEvent {
            txid,
            amount,
            fee: Amount::ZERO,
        },
    )
    .ok_or(LnurlSendDirectError::InsufficientBalance)?;

    dbtx.commit();

    Ok(operation)
}

async fn send_inner(
    ctx: &ClientContext,
    account: Account,
    gateway_pk: GatewayPk,
    gateway_info: GatewayInfo,
    invoice: Bolt11Invoice,
    max: bool,
) -> Result<OperationId, InvoiceSendError> {
    let amount = invoice
        .amount_milli_satoshis()
        .ok_or(InvoiceSendError::InvoiceMissingAmount)?;

    if invoice.is_expired() {
        return Err(InvoiceSendError::InvoiceExpired);
    }

    if ctx.config.network != invoice.currency().into() {
        return Err(InvoiceSendError::WrongCurrency {
            invoice_currency: invoice.currency(),
            mint_currency: ctx.config.network.into(),
        });
    }

    let operation = OperationId::from_encodable(&invoice.payment_hash());

    let refund_keypair = Keypair::new(secp256k1::SECP256K1, &mut rand::thread_rng());

    if !gateway_info.send_fee.is_within(&PaymentFee::SEND_FEE_LIMIT) {
        return Err(InvoiceSendError::GatewayFeeExceedsLimit);
    }

    let fee = gateway_info.send_fee.fee(amount);
    let amount = Amount(amount);

    let contract = OutgoingContract {
        payment_hash: *invoice.payment_hash(),
        amount,
        fee,
        claim_pk: gateway_info.module_public_key,
        refund_pk: refund_keypair.x_only_public_key().0,
    };

    let tx_builder = TxBuilder::from_output(Output {
        output: wire::Output::Lightning(Box::new(LightningOutput::Outgoing(contract.clone()))),
        amount: amount + fee,
        fee: ctx.config.lightning.output_fee,
    });

    let dbtx = ctx.db.begin_write();

    if dbtx
        .insert(&SendOperationIdTable, &(ctx.mint, operation), &())
        .is_some()
    {
        return Err(InvoiceSendError::InvoiceAlreadyAttempted);
    }

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
    .ok_or(InvoiceSendError::InsufficientBalance)?;

    let sm = SendStateMachine {
        common: SendSMCommon {
            account,
            operation,
            outpoint: OutPoint { txid, out_idx: 0 },
            contract,
            gateway_pk,
            invoice: LightningInvoice::Bolt11(invoice.clone()),
            refund_keypair,
        },
        state: SendSMState::Funding,
    };

    crate::executor::add_state_machine_dbtx(ctx, SendStateMachineTable, &dbtx, sm);

    dbtx.commit();

    Ok(operation)
}

/// Author the incoming contract for `recipient_pk` and fetch the invoice
/// the gateway issues against it, which has to carry the contract's
/// payment hash: the mint reporting that hash funded then means this
/// contract was.
async fn create_contract_and_fetch_invoice(
    ctx: &ClientContext,
    gateway_pk: GatewayPk,
    gateway_info: GatewayInfo,
    recipient_pk: PublicKey,
    amount: Amount,
) -> Result<Bolt11Invoice, InvoiceReceiveError> {
    if !gateway_info
        .receive_fee
        .is_within(&PaymentFee::RECEIVE_FEE_LIMIT)
    {
        return Err(InvoiceReceiveError::GatewayFeeExceedsLimit);
    }

    let fee = gateway_info.receive_fee.fee(amount.0);

    if amount
        .checked_sub(fee)
        .is_none_or(|net| net < MINIMUM_INCOMING_CONTRACT_AMOUNT)
    {
        return Err(InvoiceReceiveError::AmountTooSmall);
    }

    let contract = IncomingContract::author(&recipient_pk, amount, fee);

    let payment_hash = contract.payment_hash();

    let invoice = ctx
        .gateways
        .receive(gateway_pk, ctx.mint, contract)
        .await
        .map_err(|e| InvoiceReceiveError::FailedToConnectToGateway(e.to_string()))?;

    if invoice.payment_hash() != &payment_hash {
        return Err(InvoiceReceiveError::InvalidInvoice);
    }

    if invoice.amount_milli_satoshis() != Some(amount.0) {
        return Err(InvoiceReceiveError::IncorrectInvoiceAmount);
    }

    Ok(invoice)
}

/// Try to claim a streamed incoming contract: derive its claim key from
/// `sk` and, if it is ours, submit the claim input + log the
/// `ReceiveEvent` in the caller's dbtx (which also advances the scanner's
/// stream index atomically).
///
/// The stream carries the contract as the mint stores it, so the input
/// built here is one consensus will accept — short of the contract having
/// been spent in the meantime, which nothing local can rule out.
fn receive_incoming_contract(
    ctx: &ClientContext,
    dbtx: &WriteTx,
    account: Account,
    sk: SecretKey,
    outpoint: OutPoint,
    contract: &IncomingContract,
) {
    let Some(claim_keypair) = contract.recover(&sk) else {
        return;
    };

    let tx_builder = TxBuilder::from_input(Input {
        input: wire::Input::Lightning(LightningInput::Incoming(outpoint)),
        keypair: claim_keypair,
        amount: contract
            .claim_amount()
            .expect("Consensus only holds contracts with fee <= amount"),
        fee: ctx.config.lightning.input_fee,
    });

    // The contract's outpoint, not its payment hash: a self-payment's send
    // leg already logs under the hash, and the two legs are two operations.
    let operation = OperationId::from_encodable(&outpoint);

    let payment_hash = contract.payment_hash();
    let amount = contract.amount;
    let fee = contract.fee;

    crate::ecash::finalize_and_submit_tx(
        ctx,
        dbtx,
        account,
        operation,
        tx_builder,
        Vec::new(),
        false,
        |txid| ReceiveEvent {
            txid,
            payment_hash,
            amount,
            fee,
        },
    )
    .expect("Cannot claim input, additional funding needed");
}

/// Walks the mint-wide contract stream once, trialling every
/// account's receive key against each entry. The stream and its cursor are
/// shared, so each extra account costs one ECDH per contract rather than
/// another sweep.
async fn receive_scan(ctx: ClientContext) {
    let keys = Account::ALL.map(|account| {
        (
            account,
            ctx.secret
                .lightning_secret()
                .receive_keypair(account)
                .secret_key(),
        )
    });

    loop {
        let start = ctx
            .db
            .begin_read()
            .get(&IncomingContractStreamCursorTable, &ctx.mint)
            .unwrap_or(0);

        let (entries, next) = api::await_incoming_contracts(&ctx.api, start, BATCH).await;

        let dbtx = ctx.db.begin_write();

        for (outpoint, contract) in &entries {
            for (account, sk) in keys {
                receive_incoming_contract(&ctx, &dbtx, account, sk, *outpoint, contract);
            }
        }

        dbtx.insert(&IncomingContractStreamCursorTable, &ctx.mint, &next);

        dbtx.commit();
    }
}

/// Why `lightning invoice send` paid nothing.
#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum InvoiceSendError {
    #[error("Invoice is missing an amount")]
    InvoiceMissingAmount,
    #[error("Invoice has expired")]
    InvoiceExpired,
    #[error("A payment for this invoice has already been attempted")]
    InvoiceAlreadyAttempted,
    #[error("Gateway fee exceeds the allowed limit")]
    GatewayFeeExceedsLimit,
    #[error("Gateway is not available")]
    GatewayNotAvailable,
    #[error("The client's balance is insufficient")]
    InsufficientBalance,
    #[error("Invoice is for a different currency")]
    WrongCurrency {
        invoice_currency: Currency,
        mint_currency: Currency,
    },
    #[error("Mint is not added")]
    NotAdded,
}

/// Why `lightning invoice receive` has no invoice.
#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum InvoiceReceiveError {
    #[error("Gateway is not available")]
    GatewayNotAvailable,
    #[error("Failed to connect to gateway: {0}")]
    FailedToConnectToGateway(String),
    #[error("Gateway fee exceeds the allowed limit")]
    GatewayFeeExceedsLimit,
    #[error("Amount is too small to cover fees")]
    AmountTooSmall,
    #[error("Gateway returned an invoice for another payment hash")]
    InvalidInvoice,
    #[error("Gateway returned an invoice with incorrect amount")]
    IncorrectInvoiceAmount,
    #[error("Mint is not added")]
    NotAdded,
}

/// Why `lightning lnurl send` or `lightning lnurl send-max` paid nothing:
/// the lnurl endpoint, or any of the reasons a send of the invoice it
/// returned fails for.
#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum LnurlSendError {
    #[error("The lnurl endpoint failed: {0}")]
    Lnurl(String),
    #[error("The lnurl endpoint returned an invoice for a different amount")]
    InvoiceAmountMismatch,
    #[error(transparent)]
    Invoice(#[from] InvoiceSendError),
}

/// Why `lightning lnurl send-max-amount` has no figure.
#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum LnurlSendMaxAmountError {
    #[error("Gateway is not available")]
    GatewayNotAvailable,
    #[error("Mint is not added")]
    NotAdded,
}

/// Why `lightning lnurl send-direct` or `lightning lnurl send-direct-max`
/// paid nothing.
#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum LnurlSendDirectError {
    #[error("The lnurl does not belong to this mint")]
    NotThisMint,
    #[error("Amount is too small to be claimed")]
    AmountTooSmall,
    #[error("The client's balance is insufficient")]
    InsufficientBalance,
    #[error("Mint is not added")]
    NotAdded,
}

#[derive(Error, Debug, Clone, Eq, PartialEq, ErrorCode)]
pub enum RefreshGatewaysError {
    #[error("Failed to request gateways")]
    FailedToRequestGateways,
    #[error("Mint is not added")]
    NotAdded,
}

/// Remove every row this module owns under the caller's mint prefix.
/// Called by [`crate::Client::begin_remove_mint`] for end-of-life cleanup.
pub(crate) fn wipe_tables(dbtx: &WriteTx, mint: MintId) {
    dbtx.remove(&IncomingContractStreamCursorTable, &mint);
    dbtx.remove_prefix(&SendOperationIdTable, &mint);
    dbtx.remove_prefix(&GatewayPkTable, &mint);
    dbtx.remove_prefix(&SendStateMachineTable, &mint);
}

/// Whether any of this module's state machines for `operation` is still
/// active. Operation ids are globally unique, so no mint scoping is
/// needed — the active state machines are few, and a full scan of them
/// costs what the per-mint prefix scan did.
pub(crate) fn operation_is_active(dbtx: &ReadTx, operation: OperationId) -> bool {
    dbtx.iter(&SendStateMachineTable, |r| {
        r.any(|entry| entry.1.common.operation == operation)
    })
}

/// Notify handles for this module's state machine tables, fired on every
/// commit that writes them.
pub(crate) fn sm_notifies(db: &Database) -> Vec<Arc<Notify>> {
    vec![db.notify_for_table(&SendStateMachineTable)]
}

// ─── Flat mint-keyed surface ───────────────────────────────────────

impl Client {
    /// Every gateway the mint recommends that answered a probe, keyed by
    /// pk, with the info that prices any payment through it. Callers pick
    /// one and pass its pk into the send and receive calls. The info only
    /// changes on [`lightning_refresh_gateways`], so a fee previewed from
    /// this map is the fee a send between refreshes pays.
    ///
    /// [`lightning_refresh_gateways`]: Client::lightning_refresh_gateways
    pub fn lightning_gateways(
        &self,
        mint: MintId,
    ) -> Result<BTreeMap<GatewayPk, GatewayInfo>, NotAddedError> {
        Ok(self.ctx(mint)?.gateways.list())
    }

    /// Pay an invoice from `account` through a gateway picked from
    /// [`lightning_gateways`].
    ///
    /// [`lightning_gateways`]: Client::lightning_gateways
    pub async fn lightning_invoice_send(
        &self,
        mint: MintId,
        account: Account,
        gateway_pk: GatewayPk,
        invoice: Bolt11Invoice,
    ) -> Result<OperationId, InvoiceSendError> {
        let ctx = self.ctx(mint).map_err(|_| InvoiceSendError::NotAdded)?;

        let gateway_info = ctx
            .gateways
            .info(gateway_pk)
            .ok_or(InvoiceSendError::GatewayNotAvailable)?;

        send_inner(&ctx, account, gateway_pk, gateway_info, invoice, false).await
    }

    /// Request an invoice into `account` from a gateway picked from
    /// [`lightning_gateways`]. The eventual claim logs under an operation
    /// of its own, derived from the funded contract's outpoint; its
    /// receive event carries the payment hash that matches it to the
    /// invoice.
    ///
    /// [`lightning_gateways`]: Client::lightning_gateways
    pub async fn lightning_invoice_receive(
        &self,
        mint: MintId,
        account: Account,
        gateway_pk: GatewayPk,
        amount: Amount,
    ) -> Result<Bolt11Invoice, InvoiceReceiveError> {
        let ctx = self.ctx(mint).map_err(|_| InvoiceReceiveError::NotAdded)?;

        let gateway_info = ctx
            .gateways
            .info(gateway_pk)
            .ok_or(InvoiceReceiveError::GatewayNotAvailable)?;

        let receive_keypair = ctx.secret.lightning_secret().receive_keypair(account);

        create_contract_and_fetch_invoice(
            &ctx,
            gateway_pk,
            gateway_info,
            receive_keypair.public_key(),
            amount,
        )
        .await
    }

    /// Resolve `lnurl` to an invoice for `amount` and pay it from
    /// `account` through a gateway picked from [`lightning_gateways`].
    /// Never direct: an lnurl of this mint goes through the gateway like
    /// any other here, [`lightning_lnurl_send_direct`] is the way around
    /// it.
    ///
    /// [`lightning_gateways`]: Client::lightning_gateways
    /// [`lightning_lnurl_send_direct`]: Client::lightning_lnurl_send_direct
    pub async fn lightning_lnurl_send(
        &self,
        mint: MintId,
        account: Account,
        gateway_pk: GatewayPk,
        lnurl: &Lnurl,
        amount: Amount,
    ) -> Result<OperationId, LnurlSendError> {
        let ctx = self.ctx(mint).map_err(|_| InvoiceSendError::NotAdded)?;

        let gateway_info = ctx
            .gateways
            .info(gateway_pk)
            .ok_or(InvoiceSendError::GatewayNotAvailable)?;

        lnurl_send(
            &ctx,
            account,
            gateway_pk,
            gateway_info,
            lnurl,
            amount,
            false,
        )
        .await
    }

    /// The largest whole-sat amount a [`lightning_lnurl_send_max`] from
    /// `account` through this gateway can pay.
    ///
    /// [`lightning_lnurl_send_max`]: Client::lightning_lnurl_send_max
    pub fn lightning_lnurl_send_max_amount(
        &self,
        mint: MintId,
        account: Account,
        gateway_pk: GatewayPk,
    ) -> Result<Amount, LnurlSendMaxAmountError> {
        let ctx = self
            .ctx(mint)
            .map_err(|_| LnurlSendMaxAmountError::NotAdded)?;

        let gateway_info = ctx
            .gateways
            .info(gateway_pk)
            .ok_or(LnurlSendMaxAmountError::GatewayNotAvailable)?;

        Ok(send_max_amount(&ctx, account, Some(&gateway_info)))
    }

    /// Empty `account` to `lnurl` through a gateway picked from
    /// [`lightning_gateways`]: resolve one invoice for the max, pay it —
    /// the Lightning shape of [`onchain_send_max`]. The max needs no
    /// invoice to price, so the one invoice resolved is the one paid, for
    /// the figure that empties the account: every note goes in and no
    /// change comes back. An account that moved since the caller
    /// previewed [`lightning_lnurl_send_max_amount`] moves the payment
    /// with it — the figure is priced fresh here.
    ///
    /// [`lightning_gateways`]: Client::lightning_gateways
    /// [`onchain_send_max`]: Client::onchain_send_max
    /// [`lightning_lnurl_send_max_amount`]: Client::lightning_lnurl_send_max_amount
    pub async fn lightning_lnurl_send_max(
        &self,
        mint: MintId,
        account: Account,
        gateway_pk: GatewayPk,
        lnurl: &Lnurl,
    ) -> Result<OperationId, LnurlSendError> {
        let ctx = self.ctx(mint).map_err(|_| InvoiceSendError::NotAdded)?;

        let gateway_info = ctx
            .gateways
            .info(gateway_pk)
            .ok_or(InvoiceSendError::GatewayNotAvailable)?;

        let max = send_max_amount(&ctx, account, Some(&gateway_info));

        lnurl_send(&ctx, account, gateway_pk, gateway_info, lnurl, max, true).await
    }

    /// Pay `amount` from `account` to an lnurl of this mint, as
    /// [`lightning_lnurl_receive`] hands out, with no gateway: the
    /// recipient's incoming contract is funded straight from the account,
    /// at no fee, and the funding transaction's acceptance is the payment.
    /// Any other lnurl is refused; [`lightning_lnurl_mint`] tells ahead of
    /// time which mint an lnurl belongs to.
    ///
    /// [`lightning_lnurl_receive`]: Client::lightning_lnurl_receive
    /// [`lightning_lnurl_mint`]: Client::lightning_lnurl_mint
    pub fn lightning_lnurl_send_direct(
        &self,
        mint: MintId,
        account: Account,
        lnurl: &Lnurl,
        amount: Amount,
    ) -> Result<OperationId, LnurlSendDirectError> {
        let ctx = self.ctx(mint).map_err(|_| LnurlSendDirectError::NotAdded)?;

        lnurl_send_direct(&ctx, account, lnurl, amount, false)
    }

    /// The largest whole-sat amount a [`lightning_lnurl_send_direct_max`]
    /// from `account` can pay. No gateway fee applies, so only the account
    /// and the mint's fees set it.
    ///
    /// [`lightning_lnurl_send_direct_max`]: Client::lightning_lnurl_send_direct_max
    pub fn lightning_lnurl_send_direct_max_amount(
        &self,
        mint: MintId,
        account: Account,
    ) -> Result<Amount, NotAddedError> {
        let ctx = self.ctx(mint)?;

        Ok(send_max_amount(&ctx, account, None))
    }

    /// Empty `account` to an lnurl of this mint, as
    /// [`lightning_lnurl_send_direct`] pays it: every note goes in and
    /// no change comes back.
    ///
    /// [`lightning_lnurl_send_direct`]: Client::lightning_lnurl_send_direct
    pub fn lightning_lnurl_send_direct_max(
        &self,
        mint: MintId,
        account: Account,
        lnurl: &Lnurl,
    ) -> Result<OperationId, LnurlSendDirectError> {
        let ctx = self.ctx(mint).map_err(|_| LnurlSendDirectError::NotAdded)?;

        let max = send_max_amount(&ctx, account, None);

        lnurl_send_direct(&ctx, account, lnurl, max, true)
    }

    /// A shareable lnurl for `account`, served by `lnurl_daemon`. Nothing
    /// perishable goes into the payload, so it stays valid for as long as
    /// the mint exists.
    pub fn lightning_lnurl_receive(
        &self,
        mint: MintId,
        account: Account,
        lnurl_daemon: Url,
    ) -> Result<String, NotAddedError> {
        let ctx = self.ctx(mint)?;

        let config = &ctx.config;

        let recipient = ctx
            .secret
            .lightning_secret()
            .receive_keypair(account)
            .public_key();

        // `f + 1` nodes, sampled fresh per lnurl: enough that one is
        // honest and reachable whenever the mint itself is, and random
        // so bootstrap load spreads instead of pinning the lowest node ids.
        let nodes = config
            .nodes
            .values()
            .map(|endpoint| endpoint.iroh_pk)
            .choose_multiple(
                &mut rand::thread_rng(),
                config.nodes.to_num_nodes().one_honest(),
            );

        let info = MintInfoResponse::new(config).consensus_hash_sha256();

        let request = LnurlRequest {
            recipient,
            nodes,
            info,
        };

        let payload = picomint_base32::encode(&request);

        Ok(picomint_lnurl::encode_lnurl(&format!(
            "{lnurl_daemon}pay/{payload}"
        )))
    }

    /// The added mint an lnurl belongs to, if any: the one whose node set
    /// the lnurl commits to. From a client on that mint the lnurl is paid
    /// by [`lightning_lnurl_send_direct`], at no fee and without touching
    /// its endpoint; from any other by [`lightning_lnurl_send`].
    ///
    /// [`lightning_lnurl_send_direct`]: Client::lightning_lnurl_send_direct
    /// [`lightning_lnurl_send`]: Client::lightning_lnurl_send
    pub fn lightning_lnurl_mint(&self, lnurl: &Lnurl) -> Option<MintId> {
        let request = decode_lnurl_request(lnurl)?;

        self.mint_configs()
            .into_iter()
            .find(|entry| MintInfoResponse::new(&entry.1).consensus_hash_sha256() == request.info)
            .map(|entry| entry.0)
    }

    /// Re-run the threshold-consensus gateway query and re-probe every
    /// announced gateway, so [`lightning_gateways`] reflects the mint's
    /// current set.
    ///
    /// [`lightning_gateways`]: Client::lightning_gateways
    pub async fn lightning_refresh_gateways(
        &self,
        mint: MintId,
    ) -> Result<(), RefreshGatewaysError> {
        let ctx = self.ctx(mint).map_err(|_| RefreshGatewaysError::NotAdded)?;

        update_gateway_pks(ctx.clone()).await?;

        update_gateway_info(ctx).await;

        Ok(())
    }
}
