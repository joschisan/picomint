mod client;
mod currency;
mod db;
mod events;
mod exchange;
mod factory;
mod fountain;
mod frb_generated;
mod lnurl;
mod payout;

use std::path::PathBuf;
use std::str::FromStr;

use bitcoin::address::NetworkUnchecked;
use flutter_rust_bridge::frb;
use lightning_invoice::Bolt11Invoice;
use picomint_client::Mnemonic;
use picomint_client::mint::ECash;
use picomint_core::invite::InviteCode;
use picomint_redb::Database;

pub use client::{GatewayInfoWrapper, PicoClient};
pub use currency::{FiatCurrency, find_fiat_currency, list_fiat_currencies};
pub use events::{Notification, OperationSummary, PaymentEvent, PaymentType};
pub use factory::{PicoClientFactory, PicoContact};
pub use fountain::{ECashDecoder, ECashEncoder};
pub use lnurl::{LnurlWrapper, PayResponseWrapper, lnurl_fetch_limits, lnurl_resolve, parse_lnurl};

/// Runs once when the Rust library is initialized (before any API call).
/// rustls 0.23 refuses to pick a `CryptoProvider` automatically when more
/// than one backend is linked, and panics on the first TLS handshake
/// ("no crypto provider configured"). Install ring's provider so outbound
/// HTTPS (LNURL, exchange rates, the gateway API) works.
#[frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[frb(sync)]
pub fn word_list() -> Vec<String> {
    bip39::Language::English
        .word_list()
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[frb]
pub struct MnemonicWrapper(pub(crate) Mnemonic);

#[frb]
pub fn parse_mnemonic(words: Vec<String>) -> Option<MnemonicWrapper> {
    Mnemonic::from_str(&words.join(" "))
        .ok()
        .map(MnemonicWrapper)
}

#[frb]
pub fn generate_mnemonic() -> MnemonicWrapper {
    MnemonicWrapper(Mnemonic::generate(12).expect("12 is a valid bip39 word count"))
}

#[frb]
pub struct DatabaseWrapper(pub(crate) Database);

#[frb]
pub async fn open_database(db_path: &str) -> DatabaseWrapper {
    let db_path = PathBuf::from_str(db_path)
        .expect("db_path is a valid path")
        .join("pico.redb");

    let db = Database::open(db_path).expect("could not open database");

    DatabaseWrapper(db)
}

#[frb]
#[derive(Clone)]
pub struct InviteCodeWrapper(pub(crate) InviteCode);

#[frb(sync)]
pub fn parse_invite_code(invite: &str) -> Option<InviteCodeWrapper> {
    picomint_base32::decode::<InviteCode>(invite)
        .ok()
        .map(InviteCodeWrapper)
}

#[frb(opaque)]
#[derive(Clone)]
pub struct ECashWrapper(pub(crate) ECash);

impl ECashWrapper {
    #[frb(sync)]
    pub fn amount_sats(&self) -> i64 {
        (self.0.amount().msat / 1000) as i64
    }

    /// Federation that minted these notes — needed to look up the
    /// reissuing client when displaying ecash from history.
    #[frb(sync)]
    pub fn federation_id(&self) -> String {
        self.0.mint.to_string()
    }

    #[frb(sync)]
    pub fn to_string(&self) -> String {
        picomint_base32::encode(&self.0)
    }
}

#[frb(sync)]
pub fn parse_ecash(ecash: &str) -> Option<ECashWrapper> {
    if let Some(stripped) = ecash.strip_prefix("picomint:") {
        return parse_ecash(stripped);
    }

    picomint_base32::decode::<ECash>(ecash)
        .ok()
        .map(ECashWrapper)
}

#[frb]
pub struct Bolt11InvoiceWrapper(pub(crate) Bolt11Invoice);

impl Bolt11InvoiceWrapper {
    #[frb(sync)]
    pub fn amount_sats(&self) -> i64 {
        (self
            .0
            .amount_milli_satoshis()
            .expect("amount-bearing invoice")
            / 1000) as i64
    }
}

#[frb(sync)]
pub fn parse_bolt11_invoice(invoice: &str) -> Option<Bolt11InvoiceWrapper> {
    if let Some(invoice) = invoice.strip_prefix("lightning:") {
        return parse_bolt11_invoice(invoice);
    }

    Bolt11Invoice::from_str(invoice)
        .ok()
        .filter(|i| i.amount_milli_satoshis().is_some())
        .map(Bolt11InvoiceWrapper)
}

#[frb]
pub struct BitcoinAddressWrapper(pub(crate) bitcoin::Address<NetworkUnchecked>);

impl BitcoinAddressWrapper {
    #[frb(sync)]
    pub fn to_string(&self) -> String {
        self.0.clone().assume_checked().to_string()
    }
}

#[frb(sync)]
pub fn parse_bitcoin_address(address: &str) -> Option<BitcoinAddressWrapper> {
    if let Some(stripped) = address.strip_prefix("bitcoin:") {
        return parse_bitcoin_address(stripped);
    }

    let address = address.split('?').next().unwrap_or(address);

    bitcoin::Address::from_str(address)
        .ok()
        .map(BitcoinAddressWrapper)
}
