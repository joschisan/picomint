//! Map picomint event log entries onto the flat shapes the Dart UI consumes.
//!
//! Three projections live here:
//! - [`parse_summary`] — six trigger events (`*Send`/`*Receive`) → static
//!   [`OperationSummary`] for the recent-payments / history card.
//! - [`parse_outcome`] — terminal events → `Some(success)` for one-shot
//!   notifications. Trigger events that the federation has nothing further
//!   to do for ("immediately terminal") also return `Some(true)` here.
//! - [`parse_payment_event`] — every public picomint event → rich
//!   [`PaymentEvent`] for the per-op timeline drawer.

use std::collections::BTreeMap;

use flutter_rust_bridge::frb;
use picomint_client::ln::events::{
    ReceiveEvent as LnReceive, SendEvent as LnSend, SendFailureEvent as LnSendFailureEvent,
    SendRefundEvent, SendSuccessEvent,
};
use picomint_client::mint::{
    MintFailureEvent, MintSuccessEvent, ReceiveEvent as MintReceive, RemintEvent,
    SendEvent as MintSend, SendFailureEvent as MintSendFailureEvent,
    SendSuccessEvent as MintSendSuccessEvent,
};
use picomint_client::wallet::events::{
    ReceiveEvent as WalletReceive, SendEvent as WalletSend, SendFailureEvent,
    SendSuccessEvent as WalletSendSuccessEvent,
};
use picomint_client::{Account, TxAcceptEvent, TxCreateEvent, TxRejectEvent};
use picomint_core::bitcoin::hex::DisplayHex;
use picomint_core::config::FederationId;
use picomint_eventlog::EventLogEntry;

#[frb]
#[derive(Clone)]
pub enum PaymentType {
    Lightning,
    Bitcoin,
    Ecash,
}

/// Static card metadata derived once from the trigger event. Live status
/// updates are not folded back in — to see those, the user opens the
/// per-operation drawer which subscribes via `subscribe_payment_events`.
#[frb]
#[derive(Clone)]
pub struct OperationSummary {
    pub operation_id: String,
    pub incoming: bool,
    pub payment_type: PaymentType,
    pub amount_sats: i64,
    pub timestamp: i64,
    /// `Some(name)` if the federation is still warm at parse time;
    /// `None` if the user has since left, in which case the Dart side
    /// renders "Unknown Federation". Resolved against a snapshot of
    /// the client set — past summaries don't get re-resolved on leave.
    pub federation_name: Option<String>,
    /// Fiat value of `amount_sats` at the rate snapshotted when the payment
    /// was first observed live. `None` for operations with no stored
    /// snapshot (predating the feature, or no rate cached at the time) — the
    /// card falls back to sats.
    pub fiat_amount: Option<f64>,
    /// ISO code the `fiat_amount` is denominated in; pairs with `fiat_amount`
    /// so the Dart side can format it via `find_fiat_currency`.
    pub fiat_currency_code: Option<String>,
}

/// One-shot toast/haptic events fired by `subscribe_notifications`. Each
/// variant maps 1:1 to the picomint event whose payload alone is enough to
/// render the toast — no summary lookup needed. Anything more nuanced
/// (e.g. send completion / failure with amount) belongs in the per-op
/// timeline drawer instead.
#[frb]
#[derive(Clone)]
pub enum Notification {
    LightningReceived { amount_sats: i64 },
    OnchainReceived { amount_sats: i64 },
    LightningRefunding,
    TransactionRejected,
}

/// One-to-one mirror of every public picomint client event, flattened for
/// transport over the frb bridge. Variant names follow `<Module><Event>`
/// (e.g. `LnSend`, `MintIssuanceComplete`) so the Dart side can match the
/// picomint source on sight. All amounts are converted to sats; all hashes
/// (txids, preimages, signatures) are rendered as lowercase hex.
#[frb]
#[derive(Clone)]
pub enum PaymentEvent {
    // ── Core (transaction-layer events shared across all modules) ────────
    TxCreate {
        timestamp: i64,
        txid: String,
        change_sats: i64,
        fee_sats: i64,
    },
    TxAccept {
        timestamp: i64,
        txid: String,
    },
    TxReject {
        timestamp: i64,
        txid: String,
        error: String,
    },

    // ── Lightning (`picomint_client::ln`) ────────────────────────────────
    LnSend {
        timestamp: i64,
        txid: String,
        amount_sats: i64,
        fee_sats: i64,
    },
    LnSendSuccess {
        timestamp: i64,
        preimage: String,
    },
    LnSendRefund {
        timestamp: i64,
        txid: String,
        expired: bool,
    },
    LnSendFailure {
        timestamp: i64,
    },
    LnReceive {
        timestamp: i64,
        txid: String,
        amount_sats: i64,
        fee_sats: i64,
    },

    // ── Mint / ECash (`picomint_client::mint`) ───────────────────────────
    MintSend {
        timestamp: i64,
        amount_sats: i64,
    },
    MintSendSuccess {
        timestamp: i64,
        /// Base32-encoded ecash; the Dart side parses it back into an
        /// `ECashWrapper` on demand for the display screen. Stored as a
        /// `String` (not `ECashWrapper`) because frb can't put opaque
        /// types inside a value-typed enum without flipping the whole
        /// enum opaque.
        ecash: String,
    },
    MintSendFailure {
        timestamp: i64,
    },
    MintRemint {
        timestamp: i64,
        txid: String,
    },
    MintReceive {
        timestamp: i64,
        txid: String,
        amount_sats: i64,
    },
    MintSuccess {
        timestamp: i64,
        txid: String,
        amount_sats: i64,
    },
    MintFailure {
        timestamp: i64,
    },

    // ── Wallet / on-chain (`picomint_client::wallet`) ────────────────────
    WalletSend {
        timestamp: i64,
        txid: String,
        amount_sats: i64,
        fee_sats: i64,
    },
    WalletSendSuccess {
        timestamp: i64,
        txid: String,
    },
    WalletSendFailure {
        timestamp: i64,
    },
    WalletReceive {
        timestamp: i64,
        txid: String,
        amount_sats: i64,
        fee_sats: i64,
    },
}

/// The `(incoming, payment_type, amount_sats)` carried by the seven trigger
/// events that materialize a card. `None` for any other event. The single
/// source of truth for "is this a summary trigger", shared by `parse_summary`
/// and `is_summary_trigger` so the snapshot recorder and the card parser
/// never disagree on which operations count.
///
/// A join's restored notes need no entry of its own, and get none: they are
/// written straight into the wallet beside the counter marks, before the
/// client that would log anything exists. A restore surfaces as balance
/// rather than as history — the transactions that earned those notes belong
/// to the session that was lost.
fn trigger_fields(entry: &EventLogEntry) -> Option<(bool, PaymentType, i64)> {
    if !is_user_account(entry) {
        return None;
    }

    if let Some(e) = entry.to_event::<MintSend>() {
        return Some((false, PaymentType::Ecash, (e.amount.msat / 1000) as i64));
    }
    if let Some(e) = entry.to_event::<MintReceive>() {
        return Some((true, PaymentType::Ecash, (e.amount.msat / 1000) as i64));
    }
    if let Some(e) = entry.to_event::<LnSend>() {
        return Some((false, PaymentType::Lightning, (e.amount.msat / 1000) as i64));
    }
    if let Some(e) = entry.to_event::<LnReceive>() {
        return Some((true, PaymentType::Lightning, (e.amount.msat / 1000) as i64));
    }
    if let Some(e) = entry.to_event::<WalletSend>() {
        return Some((false, PaymentType::Bitcoin, e.amount.to_sat() as i64));
    }
    if let Some(e) = entry.to_event::<WalletReceive>() {
        return Some((true, PaymentType::Bitcoin, e.amount.to_sat() as i64));
    }
    None
}

/// `true` for entries belonging to a balance the user spends.
///
/// Every account shares one event log, so a fee account being swept appends
/// the same `SendEvent` a user's own Lightning payment does. It is not the
/// user's payment, and a card for it would be money leaving a balance they
/// were never shown. The same reasoning covers the toasts: a sweep that gets
/// rejected or refunded is for whoever charged the cut to see in the log, not
/// something to interrupt the user about.
///
/// Written as a positive match on the user's balances rather than against the
/// fee accounts by name, so it has held across upstream both adding a second
/// one and collapsing back to a single one.
///
/// Checked at all three entry points rather than only the two that can be
/// reached today. The drawer timeline needs an operation id, and only a card
/// hands one out, so filtering the cards already hides it — but that leaves
/// the invariant living in the caller. Here it holds by construction.
fn is_user_account(entry: &EventLogEntry) -> bool {
    matches!(entry.account, Account::User(_))
}

/// `true` for the trigger events that materialize an `OperationSummary` card.
/// Used by the fiat-snapshot recorder to decide which operations to price,
/// without needing the federation-name map `parse_summary` requires.
pub(crate) fn is_summary_trigger(entry: &EventLogEntry) -> bool {
    trigger_fields(entry).is_some()
}

/// Parse the trigger events that materialize a new operation in the list.
/// Every other event type returns `None`. `names` is a snapshot of
/// currently-warm federation ids → names; entries from federations the user
/// has since left resolve to `federation_name: None`. `fiat` is the
/// `(currency_code, btc_price)` snapshotted for this operation, if any —
/// converted to the displayed `fiat_amount`.
pub(crate) fn parse_summary(
    entry: &EventLogEntry,
    names: &BTreeMap<FederationId, String>,
    fiat: Option<(String, f64)>,
) -> Option<OperationSummary> {
    let (incoming, payment_type, amount_sats) = trigger_fields(entry)?;

    let (fiat_currency_code, fiat_amount) = match fiat {
        Some((code, rate)) => (
            Some(code),
            Some((amount_sats as f64 / 100_000_000.0) * rate),
        ),
        None => (None, None),
    };

    Some(OperationSummary {
        operation_id: entry.operation.to_string(),
        incoming,
        payment_type,
        amount_sats,
        timestamp: entry.timestamp as i64,
        federation_name: names.get(&entry.federation).cloned(),
        fiat_amount,
        fiat_currency_code,
    })
}

/// `Some(notification)` for events whose own payload carries everything the
/// toast needs — no `summary` cache, no extra roundtrip. Other events are
/// either internal status updates (visible only via the per-op drawer) or
/// would require summary lookup we deliberately avoid.
pub(crate) fn parse_notification(entry: &EventLogEntry) -> Option<Notification> {
    if !is_user_account(entry) {
        return None;
    }

    if let Some(e) = entry.to_event::<LnReceive>() {
        return Some(Notification::LightningReceived {
            amount_sats: (e.amount.msat / 1000) as i64,
        });
    }
    if let Some(e) = entry.to_event::<WalletReceive>() {
        return Some(Notification::OnchainReceived {
            amount_sats: e.amount.to_sat() as i64,
        });
    }
    if entry.to_event::<SendRefundEvent>().is_some() {
        return Some(Notification::LightningRefunding);
    }
    if entry.to_event::<TxRejectEvent>().is_some() {
        return Some(Notification::TransactionRejected);
    }
    None
}

/// Classify a single event log entry into a [`PaymentEvent`]. Returns
/// `None` for entries that don't correspond to any known picomint client
/// event type (forward-compatible with new modules added upstream).
pub(crate) fn parse_payment_event(entry: &EventLogEntry) -> Option<PaymentEvent> {
    if !is_user_account(entry) {
        return None;
    }

    let timestamp = entry.timestamp as i64;

    // ── Core ────────────────────────────────────────────────────────────
    if let Some(e) = entry.to_event::<TxCreateEvent>() {
        return Some(PaymentEvent::TxCreate {
            timestamp,
            txid: e.txid.to_string(),
            change_sats: (e.remint.msat / 1000) as i64,
            fee_sats: ((e.tx_fee.msat + e.app_fee.msat) / 1000) as i64,
        });
    }
    if let Some(e) = entry.to_event::<TxAcceptEvent>() {
        return Some(PaymentEvent::TxAccept {
            timestamp,
            txid: e.txid.to_string(),
        });
    }
    if let Some(e) = entry.to_event::<TxRejectEvent>() {
        return Some(PaymentEvent::TxReject {
            timestamp,
            txid: e.txid.to_string(),
            error: e.error,
        });
    }

    // ── Lightning ───────────────────────────────────────────────────────
    if let Some(e) = entry.to_event::<LnSend>() {
        return Some(PaymentEvent::LnSend {
            timestamp,
            txid: e.txid.to_string(),
            amount_sats: (e.amount.msat / 1000) as i64,
            fee_sats: (e.fee.msat / 1000) as i64,
        });
    }
    if let Some(e) = entry.to_event::<SendSuccessEvent>() {
        return Some(PaymentEvent::LnSendSuccess {
            timestamp,
            preimage: e.preimage.to_lower_hex_string(),
        });
    }
    if let Some(e) = entry.to_event::<SendRefundEvent>() {
        return Some(PaymentEvent::LnSendRefund {
            timestamp,
            txid: e.txid.to_string(),
            expired: e.expired,
        });
    }
    if entry.to_event::<LnSendFailureEvent>().is_some() {
        return Some(PaymentEvent::LnSendFailure { timestamp });
    }
    if let Some(e) = entry.to_event::<LnReceive>() {
        return Some(PaymentEvent::LnReceive {
            timestamp,
            txid: e.txid.to_string(),
            amount_sats: (e.amount.msat / 1000) as i64,
            fee_sats: (e.fee.msat / 1000) as i64,
        });
    }

    // ── Mint (ECash) ────────────────────────────────────────────────────
    if let Some(e) = entry.to_event::<MintSend>() {
        return Some(PaymentEvent::MintSend {
            timestamp,
            amount_sats: (e.amount.msat / 1000) as i64,
        });
    }
    if let Some(e) = entry.to_event::<MintSendSuccessEvent>() {
        return Some(PaymentEvent::MintSendSuccess {
            timestamp,
            ecash: e.ecash.to_string(),
        });
    }
    if entry.to_event::<MintSendFailureEvent>().is_some() {
        return Some(PaymentEvent::MintSendFailure { timestamp });
    }
    if let Some(e) = entry.to_event::<RemintEvent>() {
        return Some(PaymentEvent::MintRemint {
            timestamp,
            txid: e.txid.to_string(),
        });
    }
    if let Some(e) = entry.to_event::<MintReceive>() {
        return Some(PaymentEvent::MintReceive {
            timestamp,
            txid: e.txid.to_string(),
            amount_sats: (e.amount.msat / 1000) as i64,
        });
    }
    if let Some(e) = entry.to_event::<MintSuccessEvent>() {
        return Some(PaymentEvent::MintSuccess {
            timestamp,
            txid: e.txid.to_string(),
            amount_sats: (e.amount.msat / 1000) as i64,
        });
    }
    if entry.to_event::<MintFailureEvent>().is_some() {
        return Some(PaymentEvent::MintFailure { timestamp });
    }

    // ── Wallet (on-chain) ───────────────────────────────────────────────
    if let Some(e) = entry.to_event::<WalletSend>() {
        return Some(PaymentEvent::WalletSend {
            timestamp,
            txid: e.txid.to_string(),
            amount_sats: e.amount.to_sat() as i64,
            fee_sats: e.fee.to_sat() as i64,
        });
    }
    if let Some(e) = entry.to_event::<WalletSendSuccessEvent>() {
        return Some(PaymentEvent::WalletSendSuccess {
            timestamp,
            txid: e.txid.to_string(),
        });
    }
    if entry.to_event::<SendFailureEvent>().is_some() {
        return Some(PaymentEvent::WalletSendFailure { timestamp });
    }
    if let Some(e) = entry.to_event::<WalletReceive>() {
        return Some(PaymentEvent::WalletReceive {
            timestamp,
            txid: e.txid.to_string(),
            amount_sats: e.amount.to_sat() as i64,
            fee_sats: e.fee.to_sat() as i64,
        });
    }

    None
}
