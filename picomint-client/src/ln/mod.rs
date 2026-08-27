pub use picomint_core::ln as common;

mod api;
mod db;
pub mod events;
mod gateway;
mod secret;
mod send_sm;

use picomint_redb::WriteTx;
use std::sync::Arc;

use crate::executor::ModuleExecutor;
use crate::module::ClientContext;
use crate::task::TaskGroup;
use crate::tx::{Input, Output, TxBuilder};
use bitcoin::secp256k1;
use db::{GatewayPkTable, IncomingContractStreamIndexTable, SendOperationTable};
use gateway::Gateways;
use lightning_invoice::{Bolt11Invoice, Currency};
use picomint_core::NumPeersExt;
use picomint_core::config::FederationId;
use picomint_core::core::{Account, OperationId};
use picomint_core::ln::config::LightningConfigConsensus;
use picomint_core::ln::contracts::{IncomingContractSummary, IncomingOffer, OutgoingContract};
use picomint_core::ln::gateway::{GatewayInfo, GatewayPk, PaymentFee};
use picomint_core::ln::lnurl::LnurlRequest;
use picomint_core::ln::secret::IncomingContractSecret;
use picomint_core::ln::{
    LightningInput, LightningInvoice, LightningOutput, MINIMUM_INCOMING_CONTRACT_AMOUNT,
};
use picomint_core::methods::FederationInfoResponse;
use picomint_core::wire;

pub use self::secret::LnSecret;
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
const EXPIRY_DELTA_LIMIT: u64 = 1000;

/// A two hour buffer in case either the client or gateway go offline
const CONTRACT_CONFIRMATION_BUFFER: u64 = 12;

/// Contracts pulled per round trip when walking the incoming-contract
/// stream.
///
/// The stream is federation-wide and a cold client walks all of it, so the
/// batch sets how many round trips that costs — and each round trip fans out
/// to every guardian. A thousand summaries is ~150 kB per peer, which is a
/// reasonable unit of work against a set that only grows with the
/// federation's unclaimed contracts.
const CONTRACT_STREAM_BATCH: u64 = 1000;

pub type SendResult = Result<OperationId, SendPaymentError>;

#[derive(Clone)]
pub struct LightningClientContext {
    pub(crate) federation: FederationId,
    pub(crate) client_ctx: ClientContext,
    pub(crate) mint: Arc<crate::mint::MintClientModule>,
    pub(crate) input_fee: Amount,
    pub(crate) gateways: Gateways,
}

#[derive(Clone)]
pub struct LightningClientModule {
    federation: FederationId,
    cfg: LightningConfigConsensus,
    client_ctx: ClientContext,
    mint: Arc<crate::mint::MintClientModule>,
    secret: LnSecret,
    executor: ModuleExecutor<SendStateMachine, SendStateMachineTable>,
    // Pool of announced gateways, each holding its kept-alive connection and
    // latest probed info together. Membership is reconciled by
    // `update_gateway_pks` and info by `update_gateway_info`; the announced pk
    // set is persisted to [`GatewayPkTable`] so a cold start can warm the pool
    // without first re-running the threshold-consensus gateway query.
    gateways: Gateways,
}

impl LightningClientModule {
    pub fn input_fee(&self) -> Amount {
        self.cfg.input_fee
    }

    pub fn output_fee(&self) -> Amount {
        self.cfg.output_fee
    }

    pub fn new(
        federation: FederationId,
        cfg: LightningConfigConsensus,
        client_ctx: ClientContext,
        mint: Arc<crate::mint::MintClientModule>,
        secret: LnSecret,
        tg: &TaskGroup,
    ) -> Self {
        let gateways = Gateways::new(client_ctx.api().endpoint().clone());

        let sm_context = LightningClientContext {
            federation,
            client_ctx: client_ctx.clone(),
            mint: mint.clone(),
            input_fee: cfg.input_fee,
            gateways: gateways.clone(),
        };

        let executor = ModuleExecutor::new(
            client_ctx.db().clone(),
            SendStateMachineTable(federation),
            sm_context,
            tg.clone(),
        );

        let module = Self {
            federation,
            cfg,
            client_ctx,
            mint,
            secret,
            executor,
            gateways,
        };

        tg.spawn(Self::receive_scan(module.clone()));

        // Cold-start gateway warmup, run concurrently: the info probe reads
        // whatever pks the previous session persisted, so `select_gateway`
        // becomes usable without waiting on the threshold-consensus pk query.
        tg.spawn(Self::update_gateway_pks(module.clone()));
        tg.spawn(Self::update_gateway_info(module.clone()));

        module
    }

    /// Fetch the federation's announced gateway pk list via threshold
    /// consensus, persist it to [`GatewayPkTable`] (replacing the previous
    /// set), and reconcile the connection pool to match — a deregistered
    /// gateway is dropped here, its connection aborted. Info is filled in
    /// separately by [`Self::update_gateway_info`].
    pub async fn update_gateway_pks(
        module: LightningClientModule,
    ) -> Result<(), RefreshGatewaysError> {
        let list = module
            .client_ctx
            .api()
            .ln_gateways()
            .await
            .map_err(|_| RefreshGatewaysError::FailedToRequestGateways)?;

        let dbtx = module.client_ctx.db().begin_write();

        dbtx.delete_table(&GatewayPkTable(module.federation));

        for gateway_pk in &list {
            dbtx.insert(&GatewayPkTable(module.federation), gateway_pk, &());
        }

        dbtx.commit();

        module.gateways.reconcile(&list, true);

        Ok(())
    }

    /// Probe every gateway in [`GatewayPkTable`] and refresh its info. Ensures
    /// a connection to each (add-only — never removing a gateway, so it can run
    /// concurrently with [`Self::update_gateway_pks`]) and probes over it; a
    /// gateway that fails to answer is left unselectable.
    pub async fn update_gateway_info(module: LightningClientModule) {
        let list: Vec<GatewayPk> = module
            .client_ctx
            .db()
            .begin_read()
            .iter(&GatewayPkTable(module.federation), |it| {
                it.map(|(pk, ())| pk).collect()
            });

        module.gateways.reconcile(&list, false);

        module.gateways.probe(&list, module.federation).await;
    }

    /// Pick a gateway from the pool. With `invoice = Some(_)`, prefer a gateway
    /// whose lightning public key matches the invoice's recovered payee —
    /// that's a direct ecash swap, no LN routing. Otherwise return any gateway
    /// that has info, picked at random for load distribution.
    pub fn select_gateway(
        &self,
        invoice: Option<&Bolt11Invoice>,
    ) -> Result<(GatewayPk, GatewayInfo), SelectGatewayError> {
        self.gateways
            .select(invoice)
            .ok_or(SelectGatewayError::NoGatewaysAvailable)
    }

    /// Pay an invoice through a caller-selected gateway.
    ///
    /// The caller obtains `(gateway_pk, gateway_info)` via
    /// [`Self::select_gateway`] and inspects `gateway_info` to preview the
    /// cost before passing both back here. The library still enforces
    /// `PaymentFee::SEND_FEE_LIMIT` / `LN_FEE_LIMIT` and
    /// `EXPIRY_DELTA_LIMIT` on the supplied `gateway_info` as a
    /// backstop against an abusive gateway.
    #[allow(clippy::too_many_lines)]
    pub async fn send(
        &self,
        account: Account,
        gateway_pk: GatewayPk,
        gateway_info: GatewayInfo,
        invoice: Bolt11Invoice,
    ) -> Result<OperationId, SendPaymentError> {
        let amount = invoice
            .amount_milli_satoshis()
            .ok_or(SendPaymentError::InvoiceMissingAmount)?;

        if invoice.is_expired() {
            return Err(SendPaymentError::InvoiceExpired);
        }

        if self.client_ctx.network() != invoice.currency().into() {
            return Err(SendPaymentError::WrongCurrency {
                invoice_currency: invoice.currency(),
                federation_currency: self.client_ctx.network().into(),
            });
        }

        let operation = OperationId::from_encodable(&invoice.payment_hash());

        let refund_keypair = Keypair::new(secp256k1::SECP256K1, &mut rand::thread_rng());

        let is_direct_swap = invoice.recover_payee_pub_key() == gateway_info.lightning_public_key;

        if !gateway_info.send_fee.is_within(&PaymentFee::SEND_FEE_LIMIT) {
            return Err(SendPaymentError::GatewayFeeExceedsLimit);
        }

        if !is_direct_swap && !gateway_info.ln_fee.is_within(&PaymentFee::LN_FEE_LIMIT) {
            return Err(SendPaymentError::GatewayFeeExceedsLimit);
        }

        if EXPIRY_DELTA_LIMIT < gateway_info.expiry_delta {
            return Err(SendPaymentError::GatewayExpiryExceedsLimit);
        }

        let ln_fee = if is_direct_swap {
            Amount::ZERO
        } else {
            gateway_info.ln_fee.fee(amount)
        };

        let send_fee = gateway_info.send_fee.fee(amount);
        let amount = Amount::from_msat(amount);
        let fee = ln_fee + send_fee;

        let consensus_block_count = self
            .client_ctx
            .api()
            .ln_consensus_block_count()
            .await
            .map_err(|_| SendPaymentError::FailedToRequestBlockCount)?;

        let contract = OutgoingContract {
            payment_hash: *invoice.payment_hash(),
            amount,
            fee,
            expiry: consensus_block_count
                + gateway_info.expiry_delta
                + CONTRACT_CONFIRMATION_BUFFER,
            claim_pk: gateway_info.module_public_key,
            refund_pk: refund_keypair.x_only_public_key().0,
        };

        let tx_builder = TxBuilder::from_output(Output {
            output: wire::Output::Ln(Box::new(LightningOutput::Outgoing(contract.clone()))),
            amount: amount + fee,
            fee: self.cfg.output_fee,
        });

        let dbtx = self.client_ctx.db().begin_write();

        if dbtx
            .insert(&SendOperationTable(self.federation), &operation, &())
            .is_some()
        {
            return Err(SendPaymentError::InvoiceAlreadyAttempted);
        }

        let txid = self
            .mint
            .finalize_and_submit_tx(&dbtx, account, operation, tx_builder, |txid| SendEvent {
                txid,
                amount,
                fee,
            })
            .ok_or_else(|| SendPaymentError::FailedToFundPayment("Insufficient funds".into()))?;

        let sm = SendStateMachine {
            common: SendSMCommon {
                account,
                operation,
                outpoint: OutPoint { txid, out_idx: 0 },
                contract,
                gateway_pk: Some(gateway_pk),
                invoice: Some(LightningInvoice::Bolt11(invoice.clone())),
                refund_keypair,
            },
            state: SendSMState::Funding,
        };

        self.executor.add_state_machine_dbtx(&dbtx, sm);

        dbtx.commit();

        Ok(operation)
    }

    /// Request an invoice from a caller-selected gateway.
    ///
    /// The caller obtains `(gateway_pk, gateway_info)` via
    /// [`Self::select_gateway`] and inspects `gateway_info.receive_fee` to
    /// preview the cost before passing both back here. The library still
    /// enforces `PaymentFee::RECEIVE_FEE_LIMIT` on the supplied
    /// `gateway_info` as a backstop against an abusive gateway.
    pub async fn receive(
        &self,
        account: Account,
        gateway_pk: GatewayPk,
        gateway_info: GatewayInfo,
        amount: Amount,
    ) -> Result<Bolt11Invoice, ReceiveError> {
        let receive_keypair = self.secret.receive_keypair(account);

        self.create_offer_and_fetch_invoice(
            gateway_pk,
            gateway_info,
            receive_keypair.public_key(),
            amount,
        )
        .await
    }

    /// Create an incoming offer locked to a public key derived from the
    /// recipient's static module public key and fetch the invoice the gateway
    /// issues against it.
    async fn create_offer_and_fetch_invoice(
        &self,
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
            self.cfg.tpe_agg_pk,
            encryption_seed,
            preimage,
            preimage.consensus_hash(),
            amount,
            fee,
            claim_pk,
            ephemeral_kp.public_key(),
        );

        let invoice = self
            .gateways
            .receive(gateway_pk, self.federation, offer)
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
    /// the federation stores, so the input built here is one consensus will
    /// accept — short of the contract having been spent in the meantime,
    /// which nothing local can rule out.
    fn receive_incoming_contract(
        &self,
        dbtx: &WriteTx,
        account: Account,
        sk: SecretKey,
        summary: &IncomingContractSummary,
    ) {
        let Some((claim_keypair, agg_dk)) = summary.recover(&self.cfg.tpe_agg_pk, &sk) else {
            return;
        };

        let tx_builder = TxBuilder::from_input(Input {
            input: wire::Input::Ln(LightningInput::Incoming(summary.outpoint, agg_dk)),
            keypair: claim_keypair,
            amount: summary
                .claim_amount()
                .expect("Recovered summary has fee <= amount"),
            fee: self.cfg.input_fee,
        });

        let operation = OperationId::from_encodable(&summary.outpoint);

        let amount = summary.amount;
        let fee = summary.fee;

        self.mint
            .finalize_and_submit_tx(dbtx, account, operation, tx_builder, |txid| ReceiveEvent {
                txid,
                amount,
                fee,
            })
            .expect("Cannot claim input, additional funding needed");
    }

    /// Generate an lnurl for the client.
    ///
    /// Offline and infallible: every field is read from the federation config
    /// this client was built with, so an lnurl can be produced on a device
    /// that has never reached the network — which is precisely when someone
    /// wants to show one.
    ///
    /// Nothing perishable goes into the payload. The gateway set is resolved
    /// by the daemon at pay time, from the peer set inside the [`LnurlInfo`]
    /// that `info` pins, so an lnurl handed out today still routes after
    /// every gateway in the federation has been replaced.
    pub fn generate_lnurl(&self, account: Account, lnurl_daemon: String) -> String {
        let config = self.client_ctx.get_config();

        let recipient = self.secret.receive_keypair(account).public_key();

        // `f + 1` guardians, sampled fresh per lnurl: enough that one is
        // honest and reachable whenever the federation itself is, and random
        // so bootstrap load spreads instead of pinning the lowest peer ids.
        let guardians = config
            .peers
            .values()
            .map(|endpoint| endpoint.iroh_pk)
            .choose_multiple(
                &mut rand::thread_rng(),
                config.peers.to_num_peers().one_honest(),
            );

        let info = FederationInfoResponse::new(config).consensus_hash_sha256();

        let request = LnurlRequest {
            recipient,
            guardians,
            info,
        };

        let payload = picomint_base32::encode(&request);

        picomint_lnurl::encode_lnurl(&format!("{lnurl_daemon}pay/{payload}"))
    }

    /// Walks the federation-wide contract stream once, trialling every
    /// account's receive key against each entry. The stream and its cursor are
    /// shared, so each extra account costs one ECDH per contract rather than
    /// another sweep.
    async fn receive_scan(module: LightningClientModule) {
        let keys = Account::USER_ACCOUNTS
            .map(|account| (account, module.secret.receive_keypair(account).secret_key()));

        loop {
            let stream_index = module
                .client_ctx
                .db()
                .begin_read()
                .get(&IncomingContractStreamIndexTable(module.federation), &())
                .unwrap_or(0);

            let (entries, next_index) = module
                .client_ctx
                .api()
                .ln_await_incoming_contracts(stream_index, CONTRACT_STREAM_BATCH)
                .await;

            let dbtx = module.client_ctx.db().begin_write();

            for summary in &entries {
                for (account, sk) in keys {
                    module.receive_incoming_contract(&dbtx, account, sk, summary);
                }
            }

            dbtx.insert(
                &IncomingContractStreamIndexTable(module.federation),
                &(),
                &next_index,
            );

            dbtx.commit();
        }
    }
}

#[derive(Error, Debug, Clone, Eq, PartialEq)]
pub enum SelectGatewayError {
    #[error("No gateways are available")]
    NoGatewaysAvailable,
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
        federation_currency: Currency,
    },
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
}

#[derive(Error, Debug, Clone, Eq, PartialEq)]
pub enum RefreshGatewaysError {
    #[error("Failed to request gateways")]
    FailedToRequestGateways,
}

/// Drop every redb table this module owns under the caller's prefix.
/// Called by [`crate::Client::wipe`] for end-of-life client cleanup.
pub(crate) fn wipe_tables(dbtx: &picomint_redb::WriteTx, federation: FederationId) {
    dbtx.delete_table(&IncomingContractStreamIndexTable(federation));
    dbtx.delete_table(&SendOperationTable(federation));
    dbtx.delete_table(&GatewayPkTable(federation));
    dbtx.delete_table(&SendStateMachineTable(federation));
}
