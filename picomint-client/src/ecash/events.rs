use crate::eventlog::{Event, EventKind};
use picomint_core::Amount;
use picomint_core::TransactionId;
use picomint_core::sql::SqlRow;
use serde::{Deserialize, Serialize};

/// `ecash send` started; `ecash_send_success` follows under the same
/// operation, at once when the notes on hand made the amount up, after
/// a reissuance otherwise.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct SendEvent {
    /// The amount asked for, in msat
    pub amount: Amount,
}

impl Event for SendEvent {
    const KIND: EventKind = EventKind::from_static("ecash-send");
}

/// `ecash send` produced its bundle; the notes left the account.
#[derive(Serialize, Deserialize, Debug, Clone, SqlRow)]
pub struct SendSuccessEvent {
    /// The bundle handed out, as the base32 string `ecash receive` takes
    pub ecash: String,
}

impl Event for SendSuccessEvent {
    const KIND: EventKind = EventKind::from_static("ecash-send-success");
}

/// `ecash send` failed: the reissuance it needed was rejected or its
/// notes did not finalize; nothing left the account.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct SendFailureEvent;

impl Event for SendFailureEvent {
    const KIND: EventKind = EventKind::from_static("ecash-send-failure");
}

/// `ecash send` had to reissue notes first because the ones on hand could
/// not make the amount up exactly.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct ReissuanceEvent {
    /// The reissuance transaction, hex
    pub txid: TransactionId,
}

impl Event for ReissuanceEvent {
    const KIND: EventKind = EventKind::from_static("ecash-reissue");
}

/// `ecash receive` submitted the bundle's notes for reissuance;
/// `tx_accept` and `ecash_success` follow under the same operation.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct ReceiveEvent {
    /// The reissuance transaction, hex
    pub txid: TransactionId,
    /// The bundle's value, in msat, before the mint's fees
    pub amount: Amount,
}

impl Event for ReceiveEvent {
    const KIND: EventKind = EventKind::from_static("ecash-receive");
}

/// The mint's signatures on a transaction's new notes arrived and the notes
/// are spendable; the balance moved here.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct IssuanceSuccessEvent {
    /// The transaction the notes came out of, hex
    pub txid: TransactionId,
    /// The value of the notes issued to the account, in msat, a send's
    /// bundle included
    pub amount: Amount,
}

impl Event for IssuanceSuccessEvent {
    const KIND: EventKind = EventKind::from_static("ecash-success");
}

/// A transaction's new notes could not be finalized: the mint rejected the
/// transaction or its signatures did not verify.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct IssuanceFailureEvent;

impl Event for IssuanceFailureEvent {
    const KIND: EventKind = EventKind::from_static("ecash-failure");
}
