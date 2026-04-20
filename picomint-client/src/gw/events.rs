use picomint_core::ln::LightningInvoice;
use picomint_core::secp256k1::schnorr::Signature;
use picomint_core::{Amount, OutPoint, TransactionId};
use picomint_eventlog::{Event, EventKind, EventSource};
use serde::{Deserialize, Serialize};

// --- Outgoing payment ---

/// Emitted when the gateway accepts a send-payment request and spawns the
/// state machine to relay the outgoing HTLC.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SendEvent {
    pub outpoint: OutPoint,
    pub invoice: LightningInvoice,
}

impl Event for SendEvent {
    const SOURCE: EventSource = EventSource::Gw;
    const KIND: EventKind = EventKind::from_static("send");
}

/// Emitted when the outgoing HTLC is claimed with a preimage.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SendSuccessEvent {
    pub preimage: [u8; 32],
    pub txid: TransactionId,
}

impl Event for SendSuccessEvent {
    const SOURCE: EventSource = EventSource::Gw;
    const KIND: EventKind = EventKind::from_static("send-success");
}

/// Emitted when the outgoing payment is cancelled via a forfeit signature.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SendCancelEvent {
    pub signature: Signature,
}

impl Event for SendCancelEvent {
    const SOURCE: EventSource = EventSource::Gw;
    const KIND: EventKind = EventKind::from_static("send-cancel");
}

// --- Incoming payment ---

/// Emitted when the gateway relays an incoming HTLC into the federation.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ReceiveEvent {
    pub txid: TransactionId,
    pub amount: Amount,
}

impl Event for ReceiveEvent {
    const SOURCE: EventSource = EventSource::Gw;
    const KIND: EventKind = EventKind::from_static("receive");
}

/// Emitted when the incoming contract decrypts to the correct preimage.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ReceiveSuccessEvent {
    pub preimage: [u8; 32],
}

impl Event for ReceiveSuccessEvent {
    const SOURCE: EventSource = EventSource::Gw;
    const KIND: EventKind = EventKind::from_static("receive-success");
}

/// Emitted when guardian decryption shares are inconsistent or invalid.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ReceiveFailureEvent;

impl Event for ReceiveFailureEvent {
    const SOURCE: EventSource = EventSource::Gw;
    const KIND: EventKind = EventKind::from_static("receive-failure");
}

/// Emitted when the incoming contract decrypts but the preimage is invalid,
/// triggering a refund via a new claim tx.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ReceiveRefundEvent {
    pub txid: TransactionId,
}

impl Event for ReceiveRefundEvent {
    const SOURCE: EventSource = EventSource::Gw;
    const KIND: EventKind = EventKind::from_static("receive-refund");
}

// --- Complete (preimage revealed to LN network) ---

/// Emitted when the completion state machine has settled or cancelled the
/// HTLC towards the LN node. Only applies to externally-routed receives.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CompleteEvent;

impl Event for CompleteEvent {
    const SOURCE: EventSource = EventSource::Gw;
    const KIND: EventKind = EventKind::from_static("complete");
}
