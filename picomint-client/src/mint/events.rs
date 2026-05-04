use picomint_core::Amount;
use picomint_core::TransactionId;
use picomint_eventlog::{Event, EventKind, EventSource};
use serde::{Deserialize, Serialize};

/// Emitted when ecash is sent out-of-band.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SendEvent {
    pub amount: Amount,
    pub ecash: String,
}

impl Event for SendEvent {
    const SOURCE: EventSource = EventSource::Mint;
    const KIND: EventKind = EventKind::from_static("send");
}

/// Emitted when a send operation requires re-minting notes before the sender
/// has enough of the right denominations to send.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RemintEvent {
    pub txid: TransactionId,
}

impl Event for RemintEvent {
    const SOURCE: EventSource = EventSource::Mint;
    const KIND: EventKind = EventKind::from_static("remint");
}

/// Emitted when a receive (reissuance) operation is initiated.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ReceiveEvent {
    pub txid: TransactionId,
    pub amount: Amount,
}

impl Event for ReceiveEvent {
    const SOURCE: EventSource = EventSource::Mint;
    const KIND: EventKind = EventKind::from_static("receive");
}

/// Emitted when a mint state machine successfully finalises new notes.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MintSuccessEvent {
    pub txid: TransactionId,
}

impl Event for MintSuccessEvent {
    const SOURCE: EventSource = EventSource::Mint;
    const KIND: EventKind = EventKind::from_static("success");
}

/// Emitted when a mint state machine fails to finalise notes.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MintFailureEvent;

impl Event for MintFailureEvent {
    const SOURCE: EventSource = EventSource::Mint;
    const KIND: EventKind = EventKind::from_static("failure");
}

/// Emitted on every recovery-state checkpoint: once at `init_recovery`
/// (`index = 0`, `total = None`), once after the first driver wake-up
/// fills in the total, and once per processed slice, ending with the
/// terminal emission at `index == total`. The terminal event commits
/// in the same dbtx as the reissuance-tx submission, so the rest of
/// the operation is observable through `TxAcceptEvent` and
/// `MintSuccessEvent` (or `MintFailureEvent` / `TxRejectEvent`) under
/// the same op id.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RecoveryEvent {
    pub index: u64,
    pub total: Option<u64>,
}

impl Event for RecoveryEvent {
    const SOURCE: EventSource = EventSource::Mint;
    const KIND: EventKind = EventKind::from_static("recovery");
}
