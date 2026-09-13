//! The lightning events: one per output variant, a contract funded in
//! either direction, and one per input variant, a contract settled and
//! how. A settlement names the output that funded its contract, so a
//! contract's lifecycle is the join of `contract` on `outpoint`.

use bitcoin::hashes::sha256;
use picomint_core::secp256k1::XOnlyPublicKey;
use picomint_core::sql::SqlRow;
use picomint_core::{Amount, InPoint, OutPoint};
use serde::{Deserialize, Serialize};

use crate::consensus::eventlog::Event;

/// An outgoing contract funded: the sender locks `amount + fee` for the
/// gateway to claim against the preimage.
#[derive(Debug, Serialize, Deserialize, SqlRow)]
pub struct OutputOutgoingEvent {
    pub outpoint: OutPoint,
    pub payment_hash: sha256::Hash,
    pub amount: Amount,
    pub fee: Amount,
    pub expiry: u32,
    pub claim_pk: XOnlyPublicKey,
    pub refund_pk: XOnlyPublicKey,
}

impl Event for OutputOutgoingEvent {
    const KIND: &'static str = "lightning-output-outgoing";
}

/// An incoming contract funded: the gateway locks `amount` for the
/// recipient to claim `amount - fee` of.
#[derive(Debug, Serialize, Deserialize, SqlRow)]
pub struct OutputIncomingEvent {
    pub outpoint: OutPoint,
    pub payment_hash: sha256::Hash,
    pub amount: Amount,
    pub fee: Amount,
    pub claim_pk: XOnlyPublicKey,
    pub refund_pk: XOnlyPublicKey,
}

impl Event for OutputIncomingEvent {
    const KIND: &'static str = "lightning-output-incoming";
}

/// An outgoing contract claimed by the gateway with the preimage.
#[derive(Debug, Serialize, Deserialize, SqlRow)]
pub struct InputOutgoingClaimEvent {
    pub inpoint: InPoint,
    pub contract: OutPoint,
    pub amount: Amount,
}

impl Event for InputOutgoingClaimEvent {
    const KIND: &'static str = "lightning-input-outgoing-claim";
}

/// An outgoing contract refunded to the sender after its expiry.
#[derive(Debug, Serialize, Deserialize, SqlRow)]
pub struct InputOutgoingRefundEvent {
    pub inpoint: InPoint,
    pub contract: OutPoint,
    pub amount: Amount,
}

impl Event for InputOutgoingRefundEvent {
    const KIND: &'static str = "lightning-input-outgoing-refund";
}

/// An outgoing contract cancelled by the gateway's forfeit signature,
/// back to the sender before expiry.
#[derive(Debug, Serialize, Deserialize, SqlRow)]
pub struct InputOutgoingCancelEvent {
    pub inpoint: InPoint,
    pub contract: OutPoint,
    pub amount: Amount,
}

impl Event for InputOutgoingCancelEvent {
    const KIND: &'static str = "lightning-input-outgoing-cancel";
}

/// An incoming contract claimed by the recipient: the decryption key
/// yielded the preimage.
#[derive(Debug, Serialize, Deserialize, SqlRow)]
pub struct InputIncomingClaimEvent {
    pub inpoint: InPoint,
    pub contract: OutPoint,
    pub amount: Amount,
}

impl Event for InputIncomingClaimEvent {
    const KIND: &'static str = "lightning-input-incoming-claim";
}

/// An incoming contract refunded to the gateway: the decryption key
/// yielded no preimage.
#[derive(Debug, Serialize, Deserialize, SqlRow)]
pub struct InputIncomingRefundEvent {
    pub inpoint: InPoint,
    pub contract: OutPoint,
    pub amount: Amount,
}

impl Event for InputIncomingRefundEvent {
    const KIND: &'static str = "lightning-input-incoming-refund";
}
