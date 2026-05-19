use picomint_redb::Database;
use picomint_redb::table;

use crate::config::setup::LocalParams;
use crate::config::{ConfigGenParams, ServerConfig};

picomint_redb::consensus_value!(ServerConfig);
picomint_redb::consensus_value!(LocalParams);
picomint_redb::consensus_value!(ConfigGenParams);

table!(
    ServerConfigTable,
    () => ServerConfig,
    "server-config",
);

table!(
    LocalParamsTable,
    () => LocalParams,
    "setup-local-params",
);

table!(
    ConfigGenParamsTable,
    () => ConfigGenParams,
    "setup-config-gen-params",
);

pub async fn load_server_config(db: &Database) -> Option<ServerConfig> {
    db.begin_read().get(&ServerConfigTable, &())
}

/// Persist the finalized `ServerConfig` and drop any leftover setup-phase
/// state in the same write tx — once consensus has a config, the
/// `LocalParams` / `ConfigGenParams` entries are dead weight.
pub async fn store_server_config(db: &Database, cfg: &ServerConfig) {
    let dbtx = db.begin_write();

    assert!(
        dbtx.insert(&ServerConfigTable, &(), cfg).is_none(),
        "Server config already present in database"
    );

    dbtx.delete_table(&LocalParamsTable);
    dbtx.delete_table(&ConfigGenParamsTable);

    dbtx.commit();
}
