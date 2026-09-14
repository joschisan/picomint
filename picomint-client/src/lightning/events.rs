use crate::eventlog::{Event, EventKind};
use picomint_core::Amount;
use picomint_core::TransactionId;
use picomint_core::sql::SqlRow;
use serde::{Deserialize, Serialize};

/// `lightning send` funded the outgoing contract with `amount + fee`;
/// `lightning_send_success` or `lightning_send_refund` follows under the same
/// operation.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct SendEvent {
    /// The mint transaction that funds the contract, hex
    pub txid: TransactionId,
    /// The invoice amount, in msat
    pub amount: Amount,
    /// The gateway's fee on top, in msat
    pub fee: Amount,
}

impl Event for SendEvent {
    const KIND: EventKind = EventKind::from_static("lightning-send");
}

/// The payment went through; `ts - lightning_send.ts` is the payment latency.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct SendSuccessEvent {
    /// The preimage that proves the payment, hex
    pub preimage: [u8; 32],
}

impl Event for SendSuccessEvent {
    const KIND: EventKind = EventKind::from_static("lightning-send-success");
}

/// The payment did not happen and the contract was refunded to the account.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct SendRefundEvent {
    /// The mint transaction that refunds the contract, hex
    pub txid: TransactionId,
    /// 1 when the contract expired without the mint seeing a preimage, 0
    /// when the gateway cancelled the payment outright
    pub expired: bool,
}

impl Event for SendRefundEvent {
    const KIND: EventKind = EventKind::from_static("lightning-send-refund");
}

/// The send ended undetermined: the refund was rejected, so the gateway
/// claimed the contract, but no preimage surfaced.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct SendFailureEvent;

impl Event for SendFailureEvent {
    const KIND: EventKind = EventKind::from_static("lightning-send-failure");
}

/// An invoice from `lightning receive` was paid and the client claimed the
/// incoming contract; `amount - fee` lands in the account.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct ReceiveEvent {
    /// The mint transaction that claims the contract, hex
    pub txid: TransactionId,
    /// The invoice amount, in msat
    pub amount: Amount,
    /// The gateway's fee taken off it, in msat
    pub fee: Amount,
}

impl Event for ReceiveEvent {
    const KIND: EventKind = EventKind::from_static("lightning-receive");
}
