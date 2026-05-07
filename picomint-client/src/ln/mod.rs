pub use picomint_core::ln as common;

mod api;
mod db;
pub mod events;
mod gateway_http;
mod secret;
mod send_sm;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::executor::ModuleExecutor;
use crate::module::ClientContext;
use crate::task::TaskGroup;
use crate::tx::{Input, Output, TxBuilder};
use bitcoin::secp256k1;
use db::{GATEWAY_INFO, INCOMING_CONTRACT_STREAM_INDEX, SEND_OPERATION};
use lightning_invoice::{Bolt11Invoice, Currency};
use picomint_core::config::FederationId;
use picomint_core::core::OperationId;
use picomint_core::ln::config::LightningConfigConsensus;
use picomint_core::ln::contracts::{IncomingContract, OutgoingContract, PaymentImage};
use picomint_core::ln::gateway_api::{GatewayInfo, PaymentFee};
use picomint_core::ln::secret::IncomingContractSecret;
use picomint_core::ln::{
    Bolt11InvoiceDescription, LightningInput, LightningInvoice, LightningOutput,
    MINIMUM_INCOMING_CONTRACT_AMOUNT, lnurl,
};
use picomint_core::wire;

pub use self::secret::LnSecret;
use picomint_core::{Amount, OutPoint};
use picomint_encoding::Encodable;
use picomint_redb::WriteTxRef;
use rand::seq::IteratorRandom;
use secp256k1::{Keypair, PublicKey, SecretKey, ecdh};
use thiserror::Error;
use tpe::{AggregateDecryptionKey, derive_agg_dk};

use self::events::{ReceiveEvent, SendEvent};
use self::send_sm::{SendSMCommon, SendSMState, SendStateMachine};

/// Number of blocks until outgoing lightning contracts times out and user
/// client can refund it unilaterally
const EXPIRATION_DELTA_LIMIT: u64 = 1440;

/// A two hour buffer in case either the client or gateway go offline
const CONTRACT_CONFIRMATION_BUFFER: u64 = 12;

pub type SendResult = Result<OperationId, SendPaymentError>;

#[derive(Clone)]
pub struct LightningClientContext {
    pub(crate) federation_id: FederationId,
    pub(crate) client_ctx: ClientContext,
    pub(crate) mint: Arc<crate::mint::MintClientModule>,
    pub(crate) input_fee: Amount,
}

#[derive(Clone)]
pub struct LightningClientModule {
    federation_id: FederationId,
    cfg: LightningConfigConsensus,
    client_ctx: ClientContext,
    mint: Arc<crate::mint::MintClientModule>,
    secret: LnSecret,
    executor: ModuleExecutor<SendStateMachine>,
}

impl LightningClientModule {
    pub fn input_fee(&self) -> Amount {
        self.cfg.input_fee
    }

    pub fn output_fee(&self) -> Amount {
        self.cfg.output_fee
    }

    pub async fn new(
        federation_id: FederationId,
        cfg: LightningConfigConsensus,
        client_ctx: ClientContext,
        mint: Arc<crate::mint::MintClientModule>,
        secret: LnSecret,
        tg: &TaskGroup,
    ) -> anyhow::Result<Self> {
        let sm_context = LightningClientContext {
            federation_id,
            client_ctx: client_ctx.clone(),
            mint: mint.clone(),
            input_fee: cfg.input_fee,
        };

        let executor = ModuleExecutor::new(client_ctx.db().clone(), sm_context, tg.clone()).await;

        let module = Self {
            federation_id,
            cfg,
            client_ctx,
            mint,
            secret,
            executor,
        };

        tg.spawn(Self::receive_scan(module.clone()));

        tg.spawn(Self::refresh_gateways(module.clone()));

        Ok(module)
    }

    /// Reconcile the local `GATEWAY_INFO` cache with the federation's
    /// announced gateway list:
    ///
    /// 1. Fetch the announced list via threshold consensus.
    /// 2. Prune cache entries whose URL is no longer in the list.
    /// 3. Fetch fresh `GatewayInfo` from every URL in the list, in parallel.
    /// 4. For each URL: insert on success, remove on failure — no stale info
    ///    survives a refresh.
    ///
    /// Called once at module startup; exposed publicly so integration tests
    /// can force a refresh after registering gateways with the guardians.
    pub async fn refresh_gateways(
        module: LightningClientModule,
    ) -> Result<(), RefreshGatewaysError> {
        let list = module
            .client_ctx
            .api()
            .ln_gateways()
            .await
            .map_err(|_| RefreshGatewaysError::FailedToRequestGateways)?;

        let dbtx = module.client_ctx.db().begin_write();

        let cached: Vec<String> = dbtx.iter(&GATEWAY_INFO, |r| r.map(|(url, _)| url).collect());

        for url in cached {
            if !list.contains(&url) {
                dbtx.remove(&GATEWAY_INFO, &url);
            }
        }

        dbtx.commit();

        let handles: Vec<_> = list
            .into_iter()
            .map(|url| {
                let module = module.clone();
                tokio::spawn(async move {
                    let info = gateway_http::gateway_info(&url, &module.federation_id).await;

                    let dbtx = module.client_ctx.db().begin_write();

                    match info {
                        Ok(Some(info)) => {
                            dbtx.insert(&GATEWAY_INFO, &url, &info);
                        }
                        _ => {
                            dbtx.remove(&GATEWAY_INFO, &url);
                        }
                    }

                    dbtx.commit();
                })
            })
            .collect();

        futures::future::join_all(handles).await;

        Ok(())
    }

    /// Pick a gateway from the local cache. With `invoice = Some(_)`,
    /// prefer a gateway whose lightning public key matches the invoice's
    /// recovered payee — that's a direct ecash swap, no LN routing.
    /// Otherwise return any cached gateway, picked at random for load
    /// distribution. Synchronous: hits redb only.
    pub fn select_gateway(
        &self,
        invoice: Option<&Bolt11Invoice>,
    ) -> Result<(String, GatewayInfo), SelectGatewayError> {
        let entries: Vec<(String, GatewayInfo)> = self
            .client_ctx
            .db()
            .begin_read()
            .iter(&GATEWAY_INFO, |r| r.collect());

        if entries.is_empty() {
            return Err(SelectGatewayError::NoGatewaysAvailable);
        }

        if let Some(invoice) = invoice {
            for (url, info) in &entries {
                if info.lightning_public_key == invoice.recover_payee_pub_key() {
                    return Ok((url.clone(), info.clone()));
                }
            }
        }

        entries
            .into_iter()
            .choose(&mut rand::thread_rng())
            .map(Ok)
            .expect("entries is non-empty")
    }

    /// Pay an invoice through a caller-selected gateway.
    ///
    /// The caller obtains `(gateway_api, gateway_info)` via
    /// [`Self::select_gateway`] and inspects `gateway_info` to preview the
    /// cost before passing both back here. The library still enforces
    /// `PaymentFee::SEND_FEE_LIMIT` / `LN_FEE_LIMIT` and
    /// `EXPIRATION_DELTA_LIMIT` on the supplied `gateway_info` as a
    /// backstop against an abusive gateway.
    #[allow(clippy::too_many_lines)]
    pub async fn send(
        &self,
        gateway_api: String,
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

        let tweak: [u8; 16] = rand::Rng::r#gen(&mut rand::thread_rng());

        let refund_keypair = self.secret.refund_keypair(&tweak);

        let is_direct_swap = invoice.recover_payee_pub_key() == gateway_info.lightning_public_key;

        if !gateway_info.send_fee.le(&PaymentFee::SEND_FEE_LIMIT) {
            return Err(SendPaymentError::GatewayFeeExceedsLimit);
        }

        if !is_direct_swap && !gateway_info.ln_fee.le(&PaymentFee::LN_FEE_LIMIT) {
            return Err(SendPaymentError::GatewayFeeExceedsLimit);
        }

        if EXPIRATION_DELTA_LIMIT < gateway_info.expiration_delta {
            return Err(SendPaymentError::GatewayExpirationExceedsLimit);
        }

        let ln_fee = if is_direct_swap {
            Amount::ZERO
        } else {
            gateway_info.ln_fee.fee(amount)
        };

        let send_fee = gateway_info.send_fee.fee(amount);
        let amount = Amount::from_msats(amount);
        let fee = ln_fee + send_fee;

        let consensus_block_count = self
            .client_ctx
            .api()
            .ln_consensus_block_count()
            .await
            .map_err(|_| SendPaymentError::FailedToRequestBlockCount)?;

        let contract = OutgoingContract {
            payment_image: PaymentImage::Hash(*invoice.payment_hash()),
            amount,
            fee,
            expiration: consensus_block_count
                + gateway_info.expiration_delta
                + CONTRACT_CONFIRMATION_BUFFER,
            claim_pk: gateway_info.module_public_key,
            refund_pk: refund_keypair.x_only_public_key().0,
            tweak,
        };

        let tx_builder = TxBuilder::from_output(Output {
            output: wire::Output::Ln(Box::new(LightningOutput::Outgoing(contract.clone()))),
            amount: amount + fee,
            fee: self.cfg.output_fee,
        });

        let dbtx = self.client_ctx.db().begin_write();

        if dbtx
            .as_ref()
            .insert(&SEND_OPERATION, &operation, &())
            .is_some()
        {
            return Err(SendPaymentError::InvoiceAlreadyAttempted(operation));
        }

        let txid = self
            .mint
            .finalize_and_submit_tx(&dbtx.as_ref(), operation, tx_builder, |txid| SendEvent {
                txid,
                amount,
                fee,
            })
            .map_err(|e| SendPaymentError::FailedToFundPayment(e.to_string()))?;

        let sm = SendStateMachine {
            common: SendSMCommon {
                operation,
                outpoint: OutPoint { txid, out_idx: 0 },
                contract,
                gateway_api: Some(gateway_api),
                invoice: Some(LightningInvoice::Bolt11(invoice.clone())),
                refund_keypair,
            },
            state: SendSMState::Funding,
        };

        self.executor.add_state_machine_dbtx(&dbtx.as_ref(), sm);

        dbtx.commit();

        Ok(operation)
    }

    /// Request an invoice from a caller-selected gateway.
    ///
    /// The caller obtains `(gateway_api, gateway_info)` via
    /// [`Self::select_gateway`] and inspects `gateway_info.receive_fee` to
    /// preview the cost before passing both back here. The library still
    /// enforces `PaymentFee::RECEIVE_FEE_LIMIT` on the supplied
    /// `gateway_info` as a backstop against an abusive gateway.
    pub async fn receive(
        &self,
        gateway_api: String,
        gateway_info: GatewayInfo,
        amount: Amount,
        expiry_secs: u32,
        description: Bolt11InvoiceDescription,
    ) -> Result<Bolt11Invoice, ReceiveError> {
        let receive_keypair = self.secret.receive_keypair();

        self.create_contract_and_fetch_invoice(
            gateway_api,
            gateway_info,
            receive_keypair.public_key(),
            amount,
            expiry_secs,
            description,
        )
        .await
    }

    /// Create an incoming contract locked to a public key derived from the
    /// recipient's static module public key and fetches the corresponding
    /// invoice.
    async fn create_contract_and_fetch_invoice(
        &self,
        gateway_api: String,
        gateway_info: GatewayInfo,
        recipient_pk: PublicKey,
        amount: Amount,
        expiry_secs: u32,
        description: Bolt11InvoiceDescription,
    ) -> Result<Bolt11Invoice, ReceiveError> {
        let ephemeral_kp = Keypair::new(secp256k1::SECP256K1, &mut rand::thread_rng());

        let shared_secret = ecdh::SharedSecret::new(&recipient_pk, &ephemeral_kp.secret_key());

        let contract_secret = IncomingContractSecret::new(shared_secret.secret_bytes());

        let encryption_seed = contract_secret.encryption_seed();
        let preimage = contract_secret.preimage();
        let claim_tweak = contract_secret.claim_tweak();

        if !gateway_info.receive_fee.le(&PaymentFee::RECEIVE_FEE_LIMIT) {
            return Err(ReceiveError::GatewayFeeExceedsLimit);
        }

        let fee = gateway_info.receive_fee.fee(amount.msats);

        if amount
            .checked_sub(fee)
            .is_none_or(|net| net < MINIMUM_INCOMING_CONTRACT_AMOUNT)
        {
            return Err(ReceiveError::AmountTooSmall);
        }

        let expiration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("System time before Unix epoch")
            .as_secs()
            .saturating_add(u64::from(expiry_secs));

        let claim_pk = recipient_pk
            .mul_tweak(secp256k1::SECP256K1, &claim_tweak)
            .expect("Tweak is valid")
            .x_only_public_key()
            .0;

        let contract = IncomingContract::new(
            self.cfg.tpe_agg_pk,
            encryption_seed,
            preimage,
            PaymentImage::Hash(preimage.consensus_hash()),
            amount,
            fee,
            expiration,
            claim_pk,
            gateway_info.module_public_key,
            ephemeral_kp.public_key(),
        );

        let invoice = gateway_http::bolt11_invoice(
            &gateway_api,
            self.federation_id,
            contract.clone(),
            amount,
            description,
            expiry_secs,
        )
        .await
        .map_err(|e| ReceiveError::FailedToConnectToGateway(e.to_string()))?;

        if invoice.payment_hash() != &preimage.consensus_hash() {
            return Err(ReceiveError::InvalidInvoice);
        }

        if invoice.amount_milli_satoshis() != Some(amount.msats) {
            return Err(ReceiveError::IncorrectInvoiceAmount);
        }

        Ok(invoice)
    }

    /// Try to claim a streamed incoming contract: decrypt with the caller's
    /// secret key, and if it's ours submit the claim input + log the
    /// `ReceiveEvent` in the caller's dbtx (which also advances the scanner's
    /// stream index atomically).
    fn receive_incoming_contract(
        &self,
        dbtx: &WriteTxRef<'_>,
        sk: SecretKey,
        outpoint: OutPoint,
        contract: &IncomingContract,
    ) {
        let Some((claim_keypair, agg_dk)) = self.recover_contract_keys(sk, contract) else {
            return;
        };

        let tx_builder = TxBuilder::from_input(Input {
            input: wire::Input::Ln(LightningInput::Incoming(outpoint, agg_dk)),
            keypair: claim_keypair,
            amount: contract.commitment.amount - contract.commitment.fee,
            fee: self.cfg.input_fee,
        });

        let operation = OperationId::from_encodable(&outpoint);

        let amount = contract.commitment.amount;
        let fee = contract.commitment.fee;

        self.mint
            .finalize_and_submit_tx(dbtx, operation, tx_builder, |txid| ReceiveEvent {
                txid,
                amount,
                fee,
            })
            .expect("Cannot claim input, additional funding needed");
    }

    fn recover_contract_keys(
        &self,
        sk: SecretKey,
        contract: &IncomingContract,
    ) -> Option<(Keypair, AggregateDecryptionKey)> {
        let shared_secret =
            ecdh::SharedSecret::new(&contract.commitment.ephemeral_pk, &sk).secret_bytes();

        let contract_secret = IncomingContractSecret::new(shared_secret);

        let encryption_seed = contract_secret.encryption_seed();
        let claim_tweak = contract_secret.claim_tweak();

        let claim_keypair = sk
            .mul_tweak(&claim_tweak)
            .expect("Tweak is valid")
            .keypair(secp256k1::SECP256K1);

        if claim_keypair.x_only_public_key().0 != contract.commitment.claim_pk {
            return None; // The claim key is not derived from our pk
        }

        let agg_decryption_key = derive_agg_dk(&self.cfg.tpe_agg_pk, &encryption_seed);

        if !contract.verify_agg_decryption_key(&self.cfg.tpe_agg_pk, &agg_decryption_key) {
            return None; // The decryption key is not derived from our pk
        }

        contract.decrypt_preimage(&agg_decryption_key)?;

        Some((claim_keypair, agg_decryption_key))
    }

    /// Generate an lnurl for the client.
    pub async fn generate_lnurl(&self, recurringd: String) -> Result<String, GenerateLnurlError> {
        let gateways = self
            .client_ctx
            .api()
            .ln_gateways()
            .await
            .map_err(|_| GenerateLnurlError::FailedToRequestGateways)?;

        if gateways.is_empty() {
            return Err(GenerateLnurlError::NoGatewaysAvailable);
        }

        let receive_keypair = self.secret.receive_keypair();

        let payload = picomint_base32::encode(&lnurl::LnurlRequest {
            federation_id: self.federation_id,
            recipient_pk: receive_keypair.public_key(),
            aggregate_pk: self.cfg.tpe_agg_pk,
            gateways,
        });

        Ok(picomint_lnurl::encode_lnurl(&format!(
            "{recurringd}pay/{payload}"
        )))
    }

    async fn receive_scan(module: LightningClientModule) {
        loop {
            let stream_index = module
                .client_ctx
                .db()
                .begin_read()
                .get(&INCOMING_CONTRACT_STREAM_INDEX, &())
                .unwrap_or(0);

            let (entries, next_index) = module
                .client_ctx
                .api()
                .ln_await_incoming_contracts(stream_index, 128)
                .await;

            let sk = module.secret.receive_keypair().secret_key();

            let dbtx = module.client_ctx.db().begin_write();

            for (outpoint, contract) in &entries {
                module.receive_incoming_contract(&dbtx.as_ref(), sk, *outpoint, contract);
            }

            dbtx.insert(&INCOMING_CONTRACT_STREAM_INDEX, &(), &next_index);

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
    InvoiceAlreadyAttempted(OperationId),
    #[error("Gateway fee exceeds the allowed limit")]
    GatewayFeeExceedsLimit,
    #[error("Gateway expiration time exceeds the allowed limit")]
    GatewayExpirationExceedsLimit,
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
pub enum GenerateLnurlError {
    #[error("No gateways are available")]
    NoGatewaysAvailable,
    #[error("Failed to request gateways")]
    FailedToRequestGateways,
}

#[derive(Error, Debug, Clone, Eq, PartialEq)]
pub enum RefreshGatewaysError {
    #[error("Failed to request gateways")]
    FailedToRequestGateways,
}

/// Drop every redb table this module owns under the caller's prefix.
/// Called by [`crate::Client::wipe`] for end-of-life client cleanup.
pub(crate) fn wipe_tables(dbtx: &picomint_redb::WriteTxRef<'_>) {
    dbtx.delete_table(&GATEWAY_INFO);
    dbtx.delete_table(&INCOMING_CONTRACT_STREAM_INDEX);
    dbtx.delete_table(&SEND_OPERATION);
    dbtx.delete_table(&crate::executor::table::<SendStateMachine>());
}
