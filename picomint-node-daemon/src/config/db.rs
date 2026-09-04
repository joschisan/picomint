use picomint_redb::table;
use picomint_redb::{Database, DbRead};

use crate::config::setup::InitParams;
use crate::config::{ConfigGenParams, ServerConfig};

table!(
    ServerConfigTable,
    () => ServerConfig,
    "server-config",
);

table!(
    InitParamsTable,
    () => InitParams,
    "setup-init-params",
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
/// `InitParams` / `ConfigGenParams` entries are dead weight.
pub async fn store_server_config(db: &Database, cfg: &ServerConfig) {
    let dbtx = db.begin_write();

    assert!(
        dbtx.insert(&ServerConfigTable, &(), cfg).is_none(),
        "Server config already present in database"
    );

    dbtx.clear_table(&InitParamsTable);
    dbtx.clear_table(&ConfigGenParamsTable);

    dbtx.commit();
}
