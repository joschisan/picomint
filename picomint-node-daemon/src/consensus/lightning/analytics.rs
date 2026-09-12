//! The lightning rows of the analytics mirror: contracts funded as
//! transaction outputs, one table per direction, and contracts settled
//! as transaction inputs. An input's `contract_txid` and `contract_idx`
//! name the output that funded it, so a contract's lifecycle is the join.

use bitcoin::hashes::sha256;
use picomint_core::lightning::contracts::{IncomingContract, OutgoingContract};
use picomint_core::lightning::{LightningInput, OutgoingWitness};
use picomint_core::secp256k1::XOnlyPublicKey;
use picomint_core::sql::SqlRow;
use picomint_core::{Amount, TransactionId};

use crate::consensus::analytics::Table;

/// A contract settled as a transaction input. `kind` is how: an outgoing
/// contract is claimed by the gateway with the preimage, refunded to the
/// sender after expiry or cancelled by the gateway's signature; an
/// incoming contract is claimed by the recipient with the decryption key.
#[derive(SqlRow)]
pub struct InputRow {
    pub txid: TransactionId,
    pub position: u16,
    pub contract_txid: TransactionId,
    pub contract_idx: u16,
    pub kind: String,
}

impl Table for InputRow {
    const NAME: &'static str = "lightning_input";
    const INDEXES: &'static [&'static str] = &["txid", "contract_txid, contract_idx"];
}

/// An outgoing contract funded as a transaction output: the sender locks
/// `amount + fee` for the gateway to claim against the preimage.
#[derive(SqlRow)]
pub struct OutgoingOutputRow {
    pub txid: TransactionId,
    pub position: u16,
    pub payment_hash: sha256::Hash,
    pub amount: Amount,
    pub fee: Amount,
    pub expiry: u32,
    pub claim_pk: XOnlyPublicKey,
    pub refund_pk: XOnlyPublicKey,
}

impl Table for OutgoingOutputRow {
    const NAME: &'static str = "lightning_outgoing_output";
    const INDEXES: &'static [&'static str] = &["txid", "claim_pk"];
}

/// An incoming contract funded as a transaction output: the gateway
/// locks `amount` for the recipient to claim `amount - fee` of.
#[derive(SqlRow)]
pub struct IncomingOutputRow {
    pub txid: TransactionId,
    pub position: u16,
    pub payment_hash: sha256::Hash,
    pub amount: Amount,
    pub fee: Amount,
    pub claim_pk: XOnlyPublicKey,
    pub refund_pk: XOnlyPublicKey,
}

impl Table for IncomingOutputRow {
    const NAME: &'static str = "lightning_incoming_output";
    const INDEXES: &'static [&'static str] = &["txid", "refund_pk"];
}

pub fn input_row(txid: TransactionId, position: u16, input: &LightningInput) -> InputRow {
    let (outpoint, kind) = match input {
        LightningInput::Outgoing(outpoint, witness) => (
            outpoint,
            match witness {
                OutgoingWitness::Claim(..) => "outgoing_claim",
                OutgoingWitness::Refund => "outgoing_refund",
                OutgoingWitness::Cancel(..) => "outgoing_cancel",
            },
        ),
        LightningInput::Incoming(outpoint, ..) => (outpoint, "incoming_claim"),
    };

    InputRow {
        txid,
        position,
        contract_txid: outpoint.txid,
        contract_idx: outpoint.out_idx,
        kind: kind.to_string(),
    }
}

pub fn outgoing_output_row(
    txid: TransactionId,
    position: u16,
    contract: &OutgoingContract,
) -> OutgoingOutputRow {
    OutgoingOutputRow {
        txid,
        position,
        payment_hash: contract.payment_hash,
        amount: contract.amount,
        fee: contract.fee,
        expiry: contract.expiry,
        claim_pk: contract.claim_pk,
        refund_pk: contract.refund_pk,
    }
}

pub fn incoming_output_row(
    txid: TransactionId,
    position: u16,
    contract: &IncomingContract,
) -> IncomingOutputRow {
    IncomingOutputRow {
        txid,
        position,
        payment_hash: contract.offer.commitment.payment_hash,
        amount: contract.offer.commitment.amount,
        fee: contract.offer.commitment.fee,
        claim_pk: contract.offer.commitment.claim_pk,
        refund_pk: contract.refund_pk,
    }
}
