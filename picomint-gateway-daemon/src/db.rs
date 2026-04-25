use picomint_core::config::{ConsensusConfig, FederationId};
use picomint_core::core::OperationId;
use picomint_core::ln::LightningInvoice;
use picomint_core::ln::contracts::{IncomingContract, OutgoingContract};
use picomint_core::{Amount, OutPoint};
use picomint_encoding::{Decodable, Encodable};
use picomint_eventlog::EventLogId;
use picomint_redb::table;

table!(
    ROOT_ENTROPY,
    () => Vec<u8>,
    "root-entropy",
);

table!(
    IROH_SK,
    () => [u8; 32],
    "iroh-sk",
);

table!(
    CLIENT_CONFIG,
    FederationId => ConsensusConfig,
    "client-config",
);

table!(
    OUTGOING_CONTRACT,
    OperationId => OutgoingContractRow,
    "outgoing-contract",
);

table!(
    INCOMING_CONTRACT,
    OperationId => IncomingContractRow,
    "incoming-contract",
);

// Set of LDK-event `payment_hash`es that have been fully processed by the
// event loop (their handler committed successfully). Written atomically
// with the handler's work inside a single daemon-DB write transaction — so
// presence implies the handler ran to completion, absence on an incoming
// event means it's safe to (re-)process.
table!(
    PROCESSED_LDK_PAYMENT,
    [u8; 32] => (),
    "processed-ldk-payment",
);

// Per-federation cursor for the trailer task. Value is the next
// (unprocessed) EventLogId on that federation's client event log. Advanced
// in the same dbtx that dispatches the external side effect — so a crashed
// trailer simply re-dispatches idempotently on restart.
table!(
    TRAILER_CURSOR,
    FederationId => EventLogId,
    "trailer-cursor",
);

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct OutgoingContractRow {
    pub federation_id: FederationId,
    pub contract: OutgoingContract,
    pub outpoint: OutPoint,
    pub invoice: LightningInvoice,
}

picomint_redb::consensus_value!(OutgoingContractRow);

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct IncomingContractRow {
    pub federation_id: FederationId,
    pub contract: IncomingContract,
    pub invoice: LightningInvoice,
    pub amount: Amount,
}

picomint_redb::consensus_value!(IncomingContractRow);
