use crate::eventlog::{Event, EventKind};
use picomint_core::sql::SqlRow;
use picomint_core::{Amount, TransactionId};
use serde::{Deserialize, Serialize};
use tbs::Signature;

// --- Sender ---

/// `swap send` locked the send contract with `amount + fee` to the broker;
/// `swap_send_success` follows under the same operation once the broker has
/// funded the receive contract.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct SendEvent {
    /// The mint transaction that funds the contract, hex
    pub txid: TransactionId,
    /// The amount the recipient receives, in msat
    pub amount: Amount,
    /// The broker's fee on top, in msat
    pub fee: Amount,
}

impl Event for SendEvent {
    const KIND: EventKind = EventKind::from_static("swap-send");
}

/// The broker funded the receive contract; `ts - swap_send.ts` is the
/// swap latency.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct SendSuccessEvent {
    /// The destination mint's attestation over the receive contract, the
    /// receipt, hex
    pub attestation: Signature,
}

impl Event for SendSuccessEvent {
    const KIND: EventKind = EventKind::from_static("swap-send-success");
}

/// `swap send` paid an address of this mint directly, with one receive
/// contract output; `tx_accept` is its outcome.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct SendDirectEvent {
    /// The mint transaction that funds the contract, hex
    pub txid: TransactionId,
    /// The amount the recipient receives, in msat
    pub amount: Amount,
}

impl Event for SendDirectEvent {
    const KIND: EventKind = EventKind::from_static("swap-send-direct");
}

// --- Recipient ---

/// A receive contract for one of the client's swap addresses was funded and
/// the client claimed it; `amount` lands in the account.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct ReceiveEvent {
    /// The mint transaction that claims the contract, hex
    pub txid: TransactionId,
    /// The amount received, in msat
    pub amount: Amount,
}

impl Event for ReceiveEvent {
    const KIND: EventKind = EventKind::from_static("swap-receive");
}

// --- Broker ---

/// The broker took on a swap and funded the receive contract in the
/// destination mint; `swap_broker_success` or `swap_broker_failure`
/// follows under the same operation.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct BrokerSwapEvent {
    /// The mint transaction that funds the contract, hex
    pub txid: TransactionId,
    /// The amount the recipient receives, in msat
    pub amount: Amount,
    /// The broker's fee, in msat
    pub fee: Amount,
}

impl Event for BrokerSwapEvent {
    const KIND: EventKind = EventKind::from_static("swap-broker-swap");
}

/// The destination mint attested the receive contract and the broker
/// claimed the send contract with it. Logged under the source mint, where
/// the claim lands.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct BrokerSuccessEvent {
    /// The attestation over the receive contract, hex
    pub attestation: Signature,
    /// The mint transaction that claims the send contract, hex
    pub txid: TransactionId,
}

impl Event for BrokerSuccessEvent {
    const KIND: EventKind = EventKind::from_static("swap-broker-success");
}

/// The swap did not settle: the funding was rejected, so nothing was
/// spent, or the source mint was removed before the claim.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct BrokerFailureEvent;

impl Event for BrokerFailureEvent {
    const KIND: EventKind = EventKind::from_static("swap-broker-failure");
}
