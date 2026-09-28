use crate::eventlog::{Event, EventKind};
use picomint_core::secp256k1::schnorr::Signature;
use picomint_core::sql::SqlRow;
use picomint_core::{Amount, OutPoint, TransactionId};
use serde::{Deserialize, Serialize};

// --- Outgoing payment ---

/// The gateway took on a client's outgoing payment; `gateway_send_success`
/// or `gateway_send_cancel` follows under the same operation.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct SendEvent {
    /// The outgoing contract's outpoint in the mint, `txid:index`
    pub outpoint: OutPoint,
    /// The invoice amount, in msat
    pub amount: Amount,
    /// The gateway's fee, in msat; on a Lightning send the routing cost
    /// comes out of it
    pub fee: Amount,
}

impl Event for SendEvent {
    const KIND: EventKind = EventKind::from_static("gateway-send");
}

/// The payment went through and the gateway claimed the outgoing contract.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct SendSuccessEvent {
    /// The preimage that proves the payment, hex
    pub preimage: [u8; 32],
    /// The mint transaction that claims the contract, hex
    pub txid: TransactionId,
    /// The Lightning routing fee paid, in msat; 0 for a payment settled
    /// between two mints of this gateway
    pub lightning_fee: Amount,
}

impl Event for SendSuccessEvent {
    const KIND: EventKind = EventKind::from_static("gateway-send-success");
}

/// The gateway could not make the payment and cancelled it; the forfeit
/// signature is the only way the sender's contract comes back to it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct SendCancelEvent {
    /// The gateway's forfeit signature the sender refunds with, hex
    pub signature: Signature,
}

impl Event for SendCancelEvent {
    const KIND: EventKind = EventKind::from_static("gateway-send-cancel");
}

// --- Incoming payment ---

/// A payment for a client's invoice arrived, over Lightning or as a
/// direct swap, and the gateway funded the incoming contract and settled
/// the payment with the preimage. The funding's `tx_accept` or `tx_reject`
/// follows under the same operation.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, SqlRow)]
pub struct ReceiveEvent {
    /// The mint transaction that funds the contract, hex
    pub txid: TransactionId,
    /// The invoice amount, in msat
    pub amount: Amount,
    /// The gateway's fee kept from it, in msat
    pub fee: Amount,
}

impl Event for ReceiveEvent {
    const KIND: EventKind = EventKind::from_static("gateway-receive");
}
