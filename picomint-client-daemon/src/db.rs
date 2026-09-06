//! The daemon's one row of its own: the mnemonic the client derives from.
//! Everything else in the database belongs to the client library. The iroh
//! endpoint gets a fresh key every start, as the app's does — nothing dials
//! a client, so its transport identity is worth nothing across restarts.

use picomint_client::{Mnemonic, random_mnemonic};
use picomint_redb::{Database, DbRead, table};
use rand::rngs::OsRng;

/// The filename of the daemon's redb database inside `DATA_DIR`.
pub const DB_FILE: &str = "client.redb";

table!(
    RootEntropyTable,
    () => Vec<u8>,
    "root-entropy",
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
