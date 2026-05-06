use picomint_core::Amount;
use picomint_core::TransactionId;
use picomint_eventlog::{Event, EventKind, EventSource};
use serde::{Deserialize, Serialize};

use super::ECash;

/// Emitted immediately when a send operation is initiated, before the
/// wallet has assembled the actual ecash. On the fast path
/// `SendSuccessEvent` lands atomically in the same dbtx; on the slow
/// path it lands later, after the reissuance tx runs through consensus
/// and the mint state machine finalises notes. Slow-path observers can
/// recover the reissuance txid from the immediately-following
/// `RemintEvent` / `TxCreateEvent` under the same operation id.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SendEvent {
    pub amount: Amount,
}

impl Event for SendEvent {
    const SOURCE: EventSource = EventSource::Mint;
    const KIND: EventKind = EventKind::from_static("send");
}

/// Terminal success event for [`crate::mint::MintClientModule::send`].
/// `ecash` is the assembled bundle the caller can hand off out-of-band;
/// in the event log it serialises as the `picomint`-prefixed base32
/// string (see `ECash`'s serde impl).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SendSuccessEvent {
    pub ecash: ECash,
}

impl Event for SendSuccessEvent {
    const SOURCE: EventSource = EventSource::Mint;
    const KIND: EventKind = EventKind::from_static("send-success");
}

/// Terminal failure event for [`crate::mint::MintClientModule::send`].
/// Fires when reissuance failed (`TxRejectEvent`/`MintFailureEvent`)
/// or — defensively — when the post-reissuance NOTE table no longer
/// has the exact denominations the send needs.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SendFailureEvent;

impl Event for SendFailureEvent {
    const SOURCE: EventSource = EventSource::Mint;
    const KIND: EventKind = EventKind::from_static("send-failure");
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
    /// Total amount of notes finalized into the local note table by this
    /// state machine (sum of all issuance-request denominations).
    pub amount: Amount,
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

/// Emitted exactly once per recovery operation, in the same dbtx as
/// the reissuance-tx submission. Presence under an op id signals
/// "scan complete, reissuance in flight"; the rest of the op rides
/// the standard `TxCreateEvent` / `TxAcceptEvent` / `MintSuccessEvent`
/// path. `txid` is `None` only when the scan recovered no notes —
/// nothing to reissue, the federation isn't asked anything.
/// `amount` is the gross recovered note value (before the federation's
/// reissuance fees).
///
/// Live progress is exposed separately as a stream via
/// [`crate::mint::MintClientModule::subscribe_recovery_progress`].
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RecoveryEvent {
    pub amount: picomint_core::Amount,
    pub txid: Option<picomint_core::TransactionId>,
}

impl Event for RecoveryEvent {
    const SOURCE: EventSource = EventSource::Mint;
    const KIND: EventKind = EventKind::from_static("recovery");
}
