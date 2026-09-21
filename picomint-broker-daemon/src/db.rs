use picomint_client::{Mnemonic, random_mnemonic};
use picomint_core::core::OperationId;
use picomint_redb::{Database, DbRead, table};
use rand::rngs::OsRng;

// BIP39 entropy for the daemon's mnemonic, written once on first start.
// Drives mint-client derivation; the iroh identity lives in its own row
// below.
table!(
    RootEntropyTable,
    () => Vec<u8>,
    "root-entropy",
);

// The daemon's iroh secret key, generated once on first start —
// deliberately independent of the mnemonic. The `BrokerPk` clients connect
// to is this row's public key; a broker restored from seed alone gets a
// fresh network identity and re-registers, while its claim keys (which do
// derive from the mnemonic) restore with the funds.
table!(
    IrohSecretKeyTable,
    () => [u8; 32],
    "iroh-sk",
);

// Every swap the broker took on, by the operation it runs under, which is
// derived from the receive contract's id as the sender's is. Written in the
// dbtx that starts the swap's state machine, so a marker always has a swap
// in flight or settled behind it, and a repeated request finds the outcome
// instead of funding again.
table!(
    SwapTable,
    OperationId => (),
    "swap",
);

/// Load the persisted mnemonic, or generate and persist a fresh one on
/// first start.
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
