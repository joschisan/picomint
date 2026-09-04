use crate::eventlog::{Event, EventKind, EventSource};
use picomint_core::Amount;
use picomint_core::TransactionId;
use serde::{Deserialize, Serialize};

/// Emitted immediately when a send operation is initiated, before the
/// wallet has assembled the actual ecash. On the fast path
/// `SendSuccessEvent` lands atomically in the same dbtx; on the slow
/// path it lands later, after the reissuance tx runs through consensus
/// and the ecash state machine finalises notes. Slow-path observers can
/// recover the reissuance txid from the immediately-following
/// `RemintEvent` / `TxCreateEvent` under the same operation id.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SendEvent {
    pub amount: Amount,
}

impl Event for SendEvent {
    const SOURCE: EventSource = EventSource::Ecash;
    const KIND: EventKind = EventKind::from_static("send");
}

/// Terminal success event for [`crate::ecash::Mint::send`].
/// `ecash` is the assembled bundle as its `picomint`-prefixed base32
/// string — the exact form callers hand off and `Ecash::from_str`
/// reverses. Kept encoded so a client replaying history does not decode
/// every bundle it scrolls past. The logged bytes are unchanged from
/// when this field was typed: `Ecash`'s serde impl serialises as this
/// same string.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SendSuccessEvent {
    pub ecash: String,
}

impl Event for SendSuccessEvent {
    const SOURCE: EventSource = EventSource::Ecash;
    const KIND: EventKind = EventKind::from_static("send-success");
}

/// Terminal failure event for [`crate::ecash::Mint::send`].
/// Fires when reissuance failed (`TxRejectEvent`/`EcashFailureEvent`)
/// or — defensively — when the post-reissuance NoteTable table no longer
/// has the exact denominations the send needs.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SendFailureEvent;

impl Event for SendFailureEvent {
    const SOURCE: EventSource = EventSource::Ecash;
    const KIND: EventKind = EventKind::from_static("send-failure");
}

/// Emitted when a send operation requires re-minting notes before the sender
/// has enough of the right denominations to send.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RemintEvent {
    pub txid: TransactionId,
}

impl Event for RemintEvent {
    const SOURCE: EventSource = EventSource::Ecash;
    const KIND: EventKind = EventKind::from_static("remint");
}

/// Emitted when a receive (reissuance) operation is initiated. Also covers
/// restore, which hands its restored notes to
/// [`crate::ecash::Mint::receive`] as an ordinary bundle — the
/// two are the same operation, notes someone else may know traded for notes
/// only this wallet does.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ReceiveEvent {
    pub txid: TransactionId,
    pub amount: Amount,
}

impl Event for ReceiveEvent {
    const SOURCE: EventSource = EventSource::Ecash;
    const KIND: EventKind = EventKind::from_static("receive");
}

/// Emitted when an ecash state machine successfully finalises new notes.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct EcashSuccessEvent {
    pub txid: TransactionId,
    /// Total amount of notes finalized into the local note table by this
    /// state machine (sum of all issuance-request denominations).
    pub amount: Amount,
}

impl Event for EcashSuccessEvent {
    const SOURCE: EventSource = EventSource::Ecash;
    const KIND: EventKind = EventKind::from_static("success");
}

/// Emitted when an ecash state machine fails to finalise notes.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct EcashFailureEvent;

impl Event for EcashFailureEvent {
    const SOURCE: EventSource = EventSource::Ecash;
    const KIND: EventKind = EventKind::from_static("failure");
}
