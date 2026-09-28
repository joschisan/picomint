use bitcoin::hashes::sha256;
use lightning_invoice::Bolt11Invoice;
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

// The one LDK payment attempt an external invoice's hash ever gets at
// this gateway: the outgoing contract whose settlement it pays. Written
// when the attempt is kicked off, never removed, so a later funding of
// the same invoice is refunded on arrival and a hash never has two
// payments in flight. LDK's payment outcomes, which carry only the hash,
// resolve their outgoing contract through it.
table!(
    PaymentHashTable,
    sha256::Hash => OperationId,
    "payment-hash",
);

// The contracts registered by a receive and not yet funded, keyed by the
// operation of their payment hash. Funding a contract removes its row in
// the same write transaction, so each is funded once however its invoice
// is paid.
table!(
    IncomingContractTable,
    OperationId => IncomingContractRow,
    "incoming-contract",
);

// The `payment_hash`es of LDK events the event loop has fully processed
// (their handler committed successfully). Written atomically with the
// handler's work inside a single daemon-DB write transaction — so presence
// implies the handler ran to completion, absence on an incoming event
// means it's safe to (re-)process.
table!(
    LdkEventPaymentHashTable,
    [u8; 32] => (),
    "ldk-event-payment-hash",
);

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct OutgoingContractRow {
    pub mint: MintId,
    pub contract: contracts::OutgoingContract,
    pub outpoint: OutPoint,
    pub invoice: Bolt11Invoice,
}

#[derive(Debug, Clone, Encodable, Decodable)]
pub struct IncomingContractRow {
    pub mint: MintId,
    pub contract: contracts::IncomingContract,
    pub invoice: Bolt11Invoice,
}

/// The outgoing contract whose settlement pays the invoice with
/// `payment_hash`, with its operation, or none when the gateway holds no
/// attempt for it, as for a payment the operator made through the CLI.
pub fn outgoing_contract(
    dbtx: &impl DbRead,
    payment_hash: sha256::Hash,
) -> Option<(OperationId, OutgoingContractRow)> {
    let operation = dbtx.get(&PaymentHashTable, &payment_hash)?;

    dbtx.get(&OutgoingContractTable, &operation)
        .map(|row| (operation, row))
}

/// Delete the daemon's rows scoped to `mint` — its outgoing-contract
/// and incoming-contract rows. Runs inside the dbtx that removes the mint
/// from the client, so a surviving contract row always implies its
/// mint is added.
pub fn wipe_mint_rows(dbtx: &WriteTx, mint: MintId) {
    let outgoing = dbtx.iter(&OutgoingContractTable, |rows| {
        rows.filter(|entry| entry.1.mint == mint)
            .map(|entry| (entry.0, entry.1.contract.payment_hash))
            .collect::<Vec<_>>()
    });

    for entry in outgoing {
        dbtx.remove(&OutgoingContractTable, &entry.0);

        dbtx.remove(&PaymentHashTable, &entry.1);
    }

    let incoming = dbtx.iter(&IncomingContractTable, |rows| {
        rows.filter(|entry| entry.1.mint == mint)
            .map(|entry| entry.0)
            .collect::<Vec<_>>()
    });

    for operation in incoming {
        dbtx.remove(&IncomingContractTable, &operation);
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
