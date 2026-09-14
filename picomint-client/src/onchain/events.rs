use crate::eventlog::{Event, EventKind, EventSource};
use bitcoin::address::NetworkUnchecked;
use bitcoin::{Address, Txid};
use picomint_core::TransactionId;
use picomint_core::sql::SqlRow;
use serde::{Deserialize, Serialize};

/// `onchain send` submitted the withdrawal to the mint; `onchain_send_success`
/// follows once the mint has broadcast it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct SendEvent {
    /// The mint transaction that burns the notes, hex
    pub txid: TransactionId,
    /// The destination address
    pub address: Address<NetworkUnchecked>,
    /// The amount the destination receives, in sat
    pub amount: bitcoin::Amount,
    /// The miner fee paid on top, in sat
    pub fee: bitcoin::Amount,
}

impl Event for SendEvent {
    const SOURCE: EventSource = EventSource::Onchain;
    const KIND: EventKind = EventKind::from_static("send");
}

/// The mint broadcast the withdrawal.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct SendSuccessEvent {
    /// The bitcoin transaction id, hex
    pub txid: Txid,
}

impl Event for SendSuccessEvent {
    const SOURCE: EventSource = EventSource::Onchain;
    const KIND: EventKind = EventKind::from_static("send-success");
}

/// The mint rejected the withdrawal; the notes stayed in the account.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct SendFailureEvent;

impl Event for SendFailureEvent {
    const SOURCE: EventSource = EventSource::Onchain;
    const KIND: EventKind = EventKind::from_static("send-failure");
}

/// A deposit reached 6 confirmations and the client claimed it; the notes
/// are issued once `core_tx_accept` follows under the same operation. Logged
/// at the claim, not when the address was generated.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct ReceiveEvent {
    /// The mint transaction that claims the deposit, hex
    pub txid: TransactionId,
    /// The deposit address
    pub address: Address<NetworkUnchecked>,
    /// The deposit's value, in sat
    pub amount: bitcoin::Amount,
    /// The miner fee of the sweep into the mint wallet, in sat, taken off
    /// the deposit
    pub fee: bitcoin::Amount,
}

impl Event for ReceiveEvent {
    const SOURCE: EventSource = EventSource::Onchain;
    const KIND: EventKind = EventKind::from_static("receive");
}
