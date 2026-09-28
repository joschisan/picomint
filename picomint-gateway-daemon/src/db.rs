use bitcoin::hashes::sha256;
use picomint_client::eventlog::EventLogId;
use picomint_client::{Mnemonic, random_mnemonic};
use picomint_core::OutPoint;
use picomint_core::config::MintId;
use picomint_core::core::OperationId;
use picomint_core::lightning::contracts;
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::{Database, DbRead, WriteTx, table};
use rand::rngs::OsRng;

// BIP39 entropy for the daemon's mnemonic, written once on first start.
// Drives mint-client derivation and the LDK node seed; the iroh
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

// Keyed by the operation derived from the contract's outpoint, so every
// funding a sender submits is its own row with its own events.
table!(
    OutgoingContractTable,
    OperationId => OutgoingContractRow,
    "outgoing-contract",
);

// What the gateway has done with each payment hash. A hash moves forward
// through `Payment` once and only removing its mint takes it back to
// absent, so an invoice gets one use: every handler advances the hash here
// in the write transaction of its side effect, or refuses the request, and
// a replayed handler finds the state it left behind.
table!(
    PaymentTable,
    sha256::Hash => Payment,
    "payment",
);

// Cursor for the daemon-wide trailer task. Value is the next (unprocessed)
// `EventLogId` on the global event log. Advanced in the same dbtx that
// dispatches the external side effect — so a crashed trailer simply
// re-dispatches idempotently on restart.
table!(
    EventLogCursorTable,
    () => EventLogId,
    "event-log-cursor",
);

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct OutgoingContractRow {
    pub mint: MintId,
    pub contract: contracts::OutgoingContract,
    pub outpoint: OutPoint,
}

/// The state of one payment hash at this gateway.
#[derive(Debug, Clone, Encodable, Decodable)]
pub enum Payment {
    /// A receive issued an invoice for this contract, and nothing has
    /// funded it yet.
    Registered {
        mint: MintId,
        contract: contracts::IncomingContract,
    },
    /// A Lightning payment of the invoice funded the contract.
    Received,
    /// A direct swap funded the contract, and the outcome of that funding
    /// settles the send of `operation`.
    Swapped { operation: OperationId },
    /// The send of `operation` was handed to LDK to pay the invoice over
    /// Lightning, and LDK's outcome for the hash settles it.
    Sending { operation: OperationId },
    /// The send the hash was used for is settled.
    Settled,
}

/// Delete the daemon's rows scoped to `mint`: its outgoing-contract rows
/// and the payment hashes registered on it or settling one of its sends.
/// Runs inside the dbtx that removes the mint from the client, so a
/// surviving row always implies its mint is added.
pub fn wipe_mint_rows(dbtx: &WriteTx, mint: MintId) {
    let operations = dbtx.iter(&OutgoingContractTable, |rows| {
        rows.filter(|entry| entry.1.mint == mint)
            .map(|entry| entry.0)
            .collect::<Vec<_>>()
    });

    for operation in &operations {
        dbtx.remove(&OutgoingContractTable, operation);
    }

    let hashes = dbtx.iter(&PaymentTable, |rows| {
        rows.filter(|entry| match &entry.1 {
            Payment::Registered { mint: other, .. } => *other == mint,
            Payment::Swapped { operation } | Payment::Sending { operation } => {
                operations.contains(operation)
            }
            Payment::Received | Payment::Settled => false,
        })
        .map(|entry| entry.0)
        .collect::<Vec<_>>()
    });

    for hash in hashes {
        dbtx.remove(&PaymentTable, &hash);
    }
}

/// Load the persisted gateway mnemonic, or generate and persist a fresh one
/// on first start. The entropy drives mint-client derivation and the
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
