use picomint_client::eventlog::EventLogId;
use picomint_client::{Mnemonic, random_mnemonic};
use picomint_core::OutPoint;
use picomint_core::config::FederationId;
use picomint_core::core::OperationId;
use picomint_core::lightning::LightningInvoice;
use picomint_core::lightning::contracts;
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::{Database, DbRead, WriteTx, table};
use rand::rngs::OsRng;

// BIP39 entropy for the daemon's mnemonic, written once on first start.
// Drives federation-client derivation and the LDK node seed; the iroh
// identity lives in its own row below.
table!(
    RootEntropyTable,
    () => Vec<u8>,
    "root-entropy",
);

// The daemon's iroh secret key, generated once on first start —
// deliberately independent of the mnemonic. The `GatewayPk` clients connect
// to is this row's public key; a gateway restored from seed alone gets a
// fresh network identity and re-registers, while its contract keys (which
// do derive from the mnemonic) restore with the funds.
table!(
    IrohSecretKeyTable,
    () => [u8; 32],
    "iroh-sk",
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

/// Delete the daemon's rows scoped to `federation` — its outgoing-contract
/// and incoming-offer rows. Runs inside the dbtx that removes the federation
/// from the client, so a surviving contract row always implies its
/// federation is added.
pub fn wipe_federation_rows(dbtx: &WriteTx, federation: FederationId) {
    let outgoing = dbtx.iter(&OutgoingContractTable, |rows| {
        rows.filter(|entry| entry.1.federation == federation)
            .map(|entry| entry.0)
            .collect::<Vec<_>>()
    });

    for operation in outgoing {
        dbtx.remove(&OutgoingContractTable, &operation);
    }

    let incoming = dbtx.iter(&IncomingOfferTable, |rows| {
        rows.filter(|entry| entry.1.federation == federation)
            .map(|entry| entry.0)
            .collect::<Vec<_>>()
    });

    for operation in incoming {
        dbtx.remove(&IncomingOfferTable, &operation);
    }
}

/// Load the persisted gateway mnemonic, or generate and persist a fresh one
/// on first start. The entropy drives federation-client derivation and the
/// LDK node seed.
pub fn load_or_init_mnemonic(db: &Database) -> anyhow::Result<Mnemonic> {
    if let Some(entropy) = db.begin_read().get(&RootEntropyTable, &()) {
        return Mnemonic::from_entropy(&entropy)
            .map_err(|e| anyhow::anyhow!("Invalid stored entropy: {e}"));
    }

    let mnemonic = random_mnemonic(&mut OsRng);

    let dbtx = db.begin_write();

    dbtx.insert(&RootEntropyTable, &(), &mnemonic.to_entropy());

    dbtx.commit();

    Ok(mnemonic)
}

/// Load the persisted iroh secret key, or generate and persist a fresh one
/// on first start.
pub fn load_or_init_iroh_secret_key(db: &Database) -> iroh_base::SecretKey {
    if let Some(bytes) = db.begin_read().get(&IrohSecretKeyTable, &()) {
        return iroh_base::SecretKey::from_bytes(&bytes);
    }

    let secret_key = iroh_base::SecretKey::generate();

    let dbtx = db.begin_write();

    dbtx.insert(&IrohSecretKeyTable, &(), &secret_key.to_bytes());

    dbtx.commit();

    secret_key
}
