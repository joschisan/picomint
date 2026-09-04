pub use picomint_core::lightning as common;

mod api;
mod db;
pub mod events;
mod gateway;
mod secret;
mod send_sm;

use anyhow::Context;
use picomint_redb::{Database, DbRead, ReadTx, WriteTx};
use std::sync::Arc;
use tokio::sync::Notify;

use crate::client::Client;
use crate::context::ClientContext;
use crate::tx::{Input, Output, TxBuilder};
use bitcoin::secp256k1;
use db::{GatewayPkTable, IncomingContractStreamIndexTable, SendOperationTable};
pub(crate) use gateway::Gateways;
use lightning_invoice::{Bolt11Invoice, Currency};
use picomint_core::NumPeersExt;
use picomint_core::config::MintId;
use picomint_core::core::{Account, OperationId};
use picomint_core::lightning::contracts::{IncomingContractSummary, IncomingOffer, OutgoingContract};
use picomint_core::lightning::gateway::{GatewayInfo, GatewayPk, PaymentFee};
use picomint_core::lightning::lnurl::LnurlRequest;
use picomint_core::lightning::secret::IncomingContractSecret;
use picomint_core::lightning::{
    LightningInput, LightningInvoice, LightningOutput, MINIMUM_INCOMING_CONTRACT_AMOUNT,
};
use picomint_core::methods::MintInfoResponse;
use picomint_core::wire;

pub use self::secret::LightningSecret;
use picomint_core::{Amount, OutPoint};
use picomint_encoding::Encodable;
use rand::seq::IteratorRandom;
use secp256k1::{Keypair, PublicKey, SecretKey, ecdh};
use thiserror::Error;

use self::events::{ReceiveEvent, SendEvent};
use self::send_sm::{SendSMCommon, SendSMState, SendStateMachine, SendStateMachineTable};

/// Maximum total contract lock, in blocks, the client is willing to accept
/// from a gateway. Backstop against an abusive gateway tying funds up before
/// the unilateral refund path opens.
const EXPIRY_DELTA_LIMIT: u16 = 1000;

/// A two hour buffer in case either the client or gateway go offline
const CONTRACT_CONFIRMATION_BUFFER: u32 = 12;

/// Contracts pulled per round trip when walking the incoming-contract
/// stream.
///
/// The stream is mint-wide and a cold client walks all of it, so the
/// batch sets how many round trips that costs — and each round trip fans out
/// to every guardian. A thousand summaries is ~150 kB per peer, which is a
/// reasonable unit of work against a set that only grows with the
/// mint's unclaimed contracts.
const BATCH: u64 = 1000;

pub type SendResult = Result<OperationId, SendPaymentError>;

/// Resume this mint's persisted send state machines and start the
/// incoming-contract scan plus the cold-start gateway warmup. Called
/// exactly once, at mint bring-up.
///
/// The warmup runs concurrently: the info probe reads whatever pks the
/// previous session persisted, so `select_gateway` becomes usable without
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

/// The largest whole-sat invoice amount a max send from `account`
/// through this gateway can pay: the account's notes spent in full cover
/// the invoice, the gateway's fee and the mint's transaction fee,
/// with the sub-sat remainder donated.
fn send_max_amount(ctx: &ClientContext, account: Account, gateway_info: &GatewayInfo) -> Amount {
    crate::ecash::largest_affordable_amount(ctx, account, |amount| {
        gateway_info.send_fee.fee(amount.msat) + ctx.config.lightning.output_fee
    })
}

/// Pick any gateway from the pool that has info, at random for load
/// distribution. A gateway charges the same fee however a payment
/// settles, so there is nothing about an invoice to match a gateway
/// against — any of them prices any payment identically to itself.
pub(crate) fn select_gateway(
    ctx: &ClientContext,
) -> Result<(GatewayPk, GatewayInfo), SelectGatewayError> {
    ctx.gateways
        .select()
        .ok_or(SelectGatewayError::NoGatewaysAvailable)
}

/// Empty `account` to `lnurl` through a caller-selected gateway: resolve
/// it, size the max, pay — the Lightning shape of
/// [`crate::onchain::Wallet::send_max`]. The max needs no
/// invoice to price, so the one invoice resolved is the one paid, for
/// the figure that empties the account: every note goes in and no change
/// comes back. An account that moved since the caller previewed
/// [`send_max_amount`] moves the payment with it — the figure is
/// priced fresh here.
///
/// All or nothing: a max outside what the endpoint accepts fails at
/// invoice resolution rather than sending a clamped amount, and the
/// balance stays where it is.
pub(crate) async fn send_max(
    ctx: &ClientContext,
    account: Account,
    gateway_pk: GatewayPk,
    gateway_info: GatewayInfo,
    lnurl: &str,
) -> anyhow::Result<OperationId> {
    let url = picomint_lnurl::parse_lnurl(lnurl).context("Not a valid lnurl")?;

    let info = picomint_lnurl::request(&url)
        .await
        .map_err(anyhow::Error::msg)?;

    let max = send_max_amount(ctx, account, &gateway_info);

    let invoice = picomint_lnurl::get_invoice(&info, max.msat)
        .await
        .map_err(anyhow::Error::msg)?
        .pr;

    Ok(send_inner(ctx, account, gateway_pk, gateway_info, invoice, true).await?)
}

async fn send_inner(
    ctx: &ClientContext,
    account: Account,
    gateway_pk: GatewayPk,
    gateway_info: GatewayInfo,
    invoice: Bolt11Invoice,
    max: bool,
) -> Result<OperationId, SendPaymentError> {
    let amount = invoice
        .amount_milli_satoshis()
        .ok_or(SendPaymentError::InvoiceMissingAmount)?;

    if invoice.is_expired() {
        return Err(SendPaymentError::InvoiceExpired);
    }

    if ctx.config.network != invoice.currency().into() {
        return Err(SendPaymentError::WrongCurrency {
            invoice_currency: invoice.currency(),
            mint_currency: ctx.config.network.into(),
        });
    }

    let operation = OperationId::from_encodable(&invoice.payment_hash());

    let refund_keypair = Keypair::new(secp256k1::SECP256K1, &mut rand::thread_rng());

    if !gateway_info.send_fee.is_within(&PaymentFee::SEND_FEE_LIMIT) {
        return Err(SendPaymentError::GatewayFeeExceedsLimit);
    }

    if EXPIRY_DELTA_LIMIT < gateway_info.expiry_delta {
        return Err(SendPaymentError::GatewayExpiryExceedsLimit);
    }

    let fee = gateway_info.send_fee.fee(amount);
    let amount = Amount::from_msat(amount);

    let consensus_block_count = crate::api::block_count(&ctx.api)
        .await
        .map_err(|_| SendPaymentError::FailedToRequestBlockCount)?;

    let contract = OutgoingContract {
        payment_hash: *invoice.payment_hash(),
        amount,
        fee,
        expiry: consensus_block_count
            + u32::from(gateway_info.expiry_delta)
            + CONTRACT_CONFIRMATION_BUFFER,
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
        .insert(&SendOperationTable, &(ctx.mint, operation), &())
        .is_some()
    {
        return Err(SendPaymentError::InvoiceAlreadyAttempted);
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
    .ok_or_else(|| SendPaymentError::FailedToFundPayment("Insufficient funds".into()))?;

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

/// Create an incoming offer locked to a public key derived from the
/// recipient's static module public key and fetch the invoice the gateway
/// issues against it.
async fn create_offer_and_fetch_invoice(
    ctx: &ClientContext,
    gateway_pk: GatewayPk,
    gateway_info: GatewayInfo,
    recipient_pk: PublicKey,
    amount: Amount,
) -> Result<Bolt11Invoice, ReceiveError> {
    let ephemeral_kp = Keypair::new(secp256k1::SECP256K1, &mut rand::thread_rng());

    let shared_secret = ecdh::SharedSecret::new(&recipient_pk, &ephemeral_kp.secret_key());

    let contract_secret = IncomingContractSecret::new(shared_secret.secret_bytes());

    let encryption_seed = contract_secret.encryption_seed();
    let preimage = contract_secret.preimage();
    let claim_tweak = contract_secret.claim_tweak();

    if !gateway_info
        .receive_fee
        .is_within(&PaymentFee::RECEIVE_FEE_LIMIT)
    {
        return Err(ReceiveError::GatewayFeeExceedsLimit);
    }

    let fee = gateway_info.receive_fee.fee(amount.msat);

    if amount
        .checked_sub(fee)
        .is_none_or(|net| net < MINIMUM_INCOMING_CONTRACT_AMOUNT)
    {
        return Err(ReceiveError::AmountTooSmall);
    }

    let claim_pk = recipient_pk
        .mul_tweak(secp256k1::SECP256K1, &claim_tweak)
        .expect("Tweak is valid")
        .x_only_public_key()
        .0;

    let offer = IncomingOffer::new(
        ctx.config.lightning.tpe_agg_pk,
        encryption_seed,
        preimage,
        preimage.consensus_hash(),
        amount,
        fee,
        claim_pk,
        ephemeral_kp.public_key(),
    );

    let invoice = ctx
        .gateways
        .receive(gateway_pk, ctx.mint, offer)
        .await
        .map_err(|e| ReceiveError::FailedToConnectToGateway(e.to_string()))?;

    if invoice.payment_hash() != &preimage.consensus_hash() {
        return Err(ReceiveError::InvalidInvoice);
    }

    if invoice.amount_milli_satoshis() != Some(amount.msat) {
        return Err(ReceiveError::IncorrectInvoiceAmount);
    }

    Ok(invoice)
}

/// Try to claim a streamed incoming contract: rebuild it from `sk` and,
/// if it is ours, submit the claim input + log the `ReceiveEvent` in the
/// caller's dbtx (which also advances the scanner's stream index
/// atomically).
///
/// A summary that recovers has been proven byte-identical to the contract
/// the mint stores, so the input built here is one consensus will
/// accept — short of the contract having been spent in the meantime,
/// which nothing local can rule out.
fn receive_incoming_contract(
    ctx: &ClientContext,
    dbtx: &WriteTx,
    account: Account,
    sk: SecretKey,
    summary: &IncomingContractSummary,
) {
    let Some((claim_keypair, agg_dk)) = summary.recover(&ctx.config.lightning.tpe_agg_pk, &sk) else {
        return;
    };

    let tx_builder = TxBuilder::from_input(Input {
        input: wire::Input::Lightning(LightningInput::Incoming(summary.outpoint, agg_dk)),
        keypair: claim_keypair,
        amount: summary
            .claim_amount()
            .expect("Recovered summary has fee <= amount"),
        fee: ctx.config.lightning.input_fee,
    });

    let operation = OperationId::from_encodable(&summary.outpoint);

    let amount = summary.amount;
    let fee = summary.fee;

    crate::ecash::finalize_and_submit_tx(
        ctx,
        dbtx,
        account,
        operation,
        tx_builder,
        Vec::new(),
        false,
        |txid| ReceiveEvent { txid, amount, fee },
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
            ctx.secret.lightning_secret().receive_keypair(account).secret_key(),
        )
    });

    loop {
        let start = ctx
            .db
            .begin_read()
            .get(&IncomingContractStreamIndexTable, &ctx.mint)
            .unwrap_or(0);

        let (entries, next) = api::await_incoming_contracts(&ctx.api, start, BATCH).await;

        let dbtx = ctx.db.begin_write();

        for summary in &entries {
            for (account, sk) in keys {
                receive_incoming_contract(&ctx, &dbtx, account, sk, summary);
            }
        }

        dbtx.insert(&IncomingContractStreamIndexTable, &ctx.mint, &next);

        dbtx.commit();
    }
}

#[derive(Error, Debug, Clone, Eq, PartialEq)]
pub enum SelectGatewayError {
    #[error("No gateways are available")]
    NoGatewaysAvailable,
    #[error("Mint is not added")]
    NotAdded,
}

#[derive(Error, Debug, Clone, Eq, PartialEq)]
pub enum SendPaymentError {
    #[error("Invoice is missing an amount")]
    InvoiceMissingAmount,
    #[error("Invoice has expired")]
    InvoiceExpired,
    #[error("A payment for this invoice has already been attempted")]
    InvoiceAlreadyAttempted,
    #[error("Gateway fee exceeds the allowed limit")]
    GatewayFeeExceedsLimit,
    #[error("Gateway expiry time exceeds the allowed limit")]
    GatewayExpiryExceedsLimit,
    #[error("Failed to request block count")]
    FailedToRequestBlockCount,
    #[error("Failed to fund the payment")]
    FailedToFundPayment(String),
    #[error("Invoice is for a different currency")]
    WrongCurrency {
        invoice_currency: Currency,
        mint_currency: Currency,
    },
    #[error("Mint is not added")]
    NotAdded,
}

#[derive(Error, Debug, Clone, Eq, PartialEq)]
pub enum ReceiveError {
    #[error("Failed to connect to gateway")]
    FailedToConnectToGateway(String),
    #[error("Gateway fee exceeds the allowed limit")]
    GatewayFeeExceedsLimit,
    #[error("Amount is too small to cover fees")]
    AmountTooSmall,
    #[error("Gateway returned an invalid invoice")]
    InvalidInvoice,
    #[error("Gateway returned an invoice with incorrect amount")]
    IncorrectInvoiceAmount,
    #[error("Mint is not added")]
    NotAdded,
}

#[derive(Error, Debug, Clone, Eq, PartialEq)]
pub enum RefreshGatewaysError {
    #[error("Failed to request gateways")]
    FailedToRequestGateways,
}

/// Remove every row this module owns under the caller's mint prefix.
/// Called by [`crate::Client::remove`] for end-of-life cleanup.
pub(crate) fn wipe_tables(dbtx: &WriteTx, mint: MintId) {
    dbtx.remove(&IncomingContractStreamIndexTable, &mint);
    dbtx.remove_prefix(&SendOperationTable, &mint);
    dbtx.remove_prefix(&GatewayPkTable, &mint);
    dbtx.remove_prefix(&SendStateMachineTable, &mint);
}

/// Whether any of this module's state machines for `operation` is still
/// active under `mint`.
pub(crate) fn operation_is_active(
    dbtx: &ReadTx,
    mint: MintId,
    operation: OperationId,
) -> bool {
    dbtx.prefix(&SendStateMachineTable, &mint, |r| {
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
    /// Pick a gateway from the mint's pool, at random for load
    /// distribution. The returned info prices any payment identically, so
    /// callers preview it and pass both values back into the send/receive
    /// calls.
    pub fn lightning_select_gateway(
        &self,
        mint: MintId,
    ) -> Result<(GatewayPk, GatewayInfo), SelectGatewayError> {
        let ctx = self
            .ctx(mint)
            .map_err(|_| SelectGatewayError::NotAdded)?;

        select_gateway(&ctx)
    }

    /// Pay an invoice from `account` through a caller-selected gateway
    /// obtained via [`lightning_select_gateway`].
    pub async fn lightning_send(
        &self,
        mint: MintId,
        account: Account,
        gateway_pk: GatewayPk,
        gateway_info: GatewayInfo,
        invoice: Bolt11Invoice,
    ) -> Result<OperationId, SendPaymentError> {
        let ctx = self
            .ctx(mint)
            .map_err(|_| SendPaymentError::NotAdded)?;

        send_inner(&ctx, account, gateway_pk, gateway_info, invoice, false).await
    }

    /// The largest whole-sat invoice amount a [`lightning_send_max`] from
    /// `account` through this gateway can pay.
    pub fn lightning_send_max_amount(
        &self,
        mint: MintId,
        account: Account,
        gateway_info: &GatewayInfo,
    ) -> anyhow::Result<Amount> {
        let ctx = self.ctx(mint)?;

        Ok(send_max_amount(&ctx, account, gateway_info))
    }

    /// Empty `account` to `lnurl` through a caller-selected gateway: resolve
    /// it, size the max, pay.
    pub async fn lightning_send_max(
        &self,
        mint: MintId,
        account: Account,
        gateway_pk: GatewayPk,
        gateway_info: GatewayInfo,
        lnurl: &str,
    ) -> anyhow::Result<OperationId> {
        let ctx = self.ctx(mint)?;

        send_max(&ctx, account, gateway_pk, gateway_info, lnurl).await
    }

    /// Request an invoice into `account` from a caller-selected gateway
    /// obtained via [`lightning_select_gateway`].
    pub async fn lightning_receive(
        &self,
        mint: MintId,
        account: Account,
        gateway_pk: GatewayPk,
        gateway_info: GatewayInfo,
        amount: Amount,
    ) -> Result<Bolt11Invoice, ReceiveError> {
        let ctx = self.ctx(mint).map_err(|_| ReceiveError::NotAdded)?;

        let receive_keypair = ctx.secret.lightning_secret().receive_keypair(account);

        create_offer_and_fetch_invoice(
            &ctx,
            gateway_pk,
            gateway_info,
            receive_keypair.public_key(),
            amount,
        )
        .await
    }

    /// A shareable lnurl for `account`, served by `lnurl_daemon`. Nothing
    /// perishable goes into the payload, so it stays valid for as long as
    /// the mint exists.
    pub fn lightning_generate_lnurl(
        &self,
        mint: MintId,
        account: Account,
        lnurl_daemon: String,
    ) -> anyhow::Result<String> {
        let ctx = self.ctx(mint)?;

        let config = &ctx.config;

        let recipient = ctx.secret.lightning_secret().receive_keypair(account).public_key();

        // `f + 1` guardians, sampled fresh per lnurl: enough that one is
        // honest and reachable whenever the mint itself is, and random
        // so bootstrap load spreads instead of pinning the lowest peer ids.
        let guardians = config
            .peers
            .values()
            .map(|endpoint| endpoint.iroh_pk)
            .choose_multiple(
                &mut rand::thread_rng(),
                config.peers.to_num_peers().one_honest(),
            );

        let info = MintInfoResponse::new(config).consensus_hash_sha256();

        let request = LnurlRequest {
            recipient,
            guardians,
            info,
        };

        let payload = picomint_base32::encode(&request);

        Ok(picomint_lnurl::encode_lnurl(&format!(
            "{lnurl_daemon}pay/{payload}"
        )))
    }

    /// Re-run the threshold-consensus gateway query and re-probe every
    /// announced gateway, so [`lightning_select_gateway`] reflects the
    /// mint's current set.
    pub async fn lightning_refresh_gateways(&self, mint: MintId) -> anyhow::Result<()> {
        let ctx = self.ctx(mint)?;

        update_gateway_pks(ctx.clone()).await?;

        update_gateway_info(ctx).await;

        Ok(())
    }
}
