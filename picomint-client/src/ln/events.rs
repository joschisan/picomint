use picomint_core::Amount;
use picomint_core::TransactionId;
use picomint_eventlog::{Event, EventKind, EventSource};
use serde::{Deserialize, Serialize};

/// Emitted when a send operation is created. `amount` is the invoice
/// amount; `fee` is the gateway's combined cut (LN routing + tx fee).
/// The client funded the underlying contract with `amount + fee`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SendEvent {
    pub txid: TransactionId,
    pub amount: Amount,
    pub fee: Amount,
}

impl Event for SendEvent {
    const SOURCE: EventSource = EventSource::Ln;
    const KIND: EventKind = EventKind::from_static("send");
}

/// Emitted when the payment successfully resolves and the preimage is known.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SendSuccessEvent {
    pub preimage: [u8; 32],
}

impl Event for SendSuccessEvent {
    const SOURCE: EventSource = EventSource::Ln;
    const KIND: EventKind = EventKind::from_static("send-success");
}

/// Emitted when the payment fails and funds are refunded via a new claim tx.
/// `expired` is `true` when the contract expired without the federation
/// observing a preimage, `false` when the gateway returned a signed cancel
/// (payment definitively did not happen).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SendRefundEvent {
    pub txid: TransactionId,
    pub expired: bool,
}

impl Event for SendRefundEvent {
    const SOURCE: EventSource = EventSource::Ln;
    const KIND: EventKind = EventKind::from_static("send-refund");
}

/// Emitted when a send is in an unrecoverable indeterminate state: the
/// refund tx was rejected (so the contract was claimed by the gateway),
/// but the federation hasn't surfaced a preimage we can verify either.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SendFailureEvent;

impl Event for SendFailureEvent {
    const SOURCE: EventSource = EventSource::Ln;
    const KIND: EventKind = EventKind::from_static("send-failure");
}

/// Emitted when a receive operation successfully claims the incoming
/// contract. `amount` is the invoice amount; `fee` is the gateway's
/// combined cut. The client received `amount - fee` ecash.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ReceiveEvent {
    pub txid: TransactionId,
    pub amount: Amount,
    pub fee: Amount,
}

impl Event for ReceiveEvent {
    const SOURCE: EventSource = EventSource::Ln;
    const KIND: EventKind = EventKind::from_static("receive");
}
