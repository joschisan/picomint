use picomint_client::Mnemonic;
use picomint_core::OutPoint;
use picomint_core::config::FederationId;
use picomint_core::core::OperationId;
use picomint_core::ln::LightningInvoice;
use picomint_core::ln::contracts;
use picomint_encoding::{Decodable, Encodable};
use picomint_eventlog::EventLogId;
use picomint_sqlite::{DbRead, table};

// BIP39 entropy for the daemon's mnemonic, written once on first start.
// Drives both federation-client derivation and the iroh secret key, so the
// `GatewayPk` (iroh node id) is reproducible from this row alone.
table!(
    RootEntropyTable,
    () => Vec<u8>,
    "root-entropy",
);

// Set of federation ids whose public-facing endpoints (`gateway_info`,
// `receive`) are gated off. Disable stops *new* client-initiated
// work; back doors (LDK event handlers, trailer, terminal settlement of
// in-flight contracts) stay open so existing operations drain naturally.
table!(
    DisabledFederationTable,
    FederationId => (),
    "disabled-federation",
);

table!(
    OutgoingContractTable,
    OperationId => OutgoingContractRow,
    "outgoing-contract",
);

table!(
    IncomingOfferTable,
    OperationId => IncomingOfferRow,
    "incoming-offer",
);

// Set of LDK-event `payment_hash`es that have been fully processed by the
// event loop (their handler committed successfully). Written atomically
// with the handler's work inside a single daemon-DB write transaction — so
// presence implies the handler ran to completion, absence on an incoming
// event means it's safe to (re-)process.
table!(
    ProcessedLdkEventTable,
    [u8; 32] => (),
    "processed-ldk-event",
);

// Cursor for the daemon-wide trailer task. Value is the next (unprocessed)
// `EventLogId` on the global event log. Advanced in the same dbtx that
// dispatches the external side effect — so a crashed trailer simply
// re-dispatches idempotently on restart.
table!(
    EventCursorTable,
    () => EventLogId,
    "event-cursor",
);

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct OutgoingContractRow {
    pub federation: FederationId,
    pub contract: contracts::OutgoingContract,
    pub outpoint: OutPoint,
    pub invoice: LightningInvoice,
}

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct IncomingOfferRow {
    pub federation: FederationId,
    pub offer: contracts::IncomingOffer,
    pub invoice: LightningInvoice,
}

/// Load the persisted gateway mnemonic, or generate and persist a fresh one
/// on first start. The entropy drives both federation-client derivation and
/// the iroh secret key, so the gateway's identity is reproducible from this
/// row alone.
pub fn load_or_init_mnemonic(db: &picomint_sqlite::Database) -> anyhow::Result<Mnemonic> {
    if let Some(entropy) = db.begin_read().get(&RootEntropyTable, &()) {
        return Mnemonic::from_entropy(&entropy)
            .map_err(|e| anyhow::anyhow!("Invalid stored entropy: {e}"));
    }

    let mnemonic = picomint_client::random_mnemonic(&mut rand::rngs::OsRng);

    let dbtx = db.begin_write();

    dbtx.insert(&RootEntropyTable, &(), &mnemonic.to_entropy());

    dbtx.commit();

    Ok(mnemonic)
}
