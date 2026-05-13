use picomint_redb::Database;
use picomint_redb::table;

use crate::config::ServerConfig;

picomint_redb::consensus_value!(ServerConfig);

table!(
    ServerConfigTable,
    () => ServerConfig,
    "server-config",
);

pub async fn load_server_config(db: &Database) -> Option<ServerConfig> {
    db.begin_read().get(&ServerConfigTable, &())
}

pub async fn store_server_config(db: &Database, cfg: &ServerConfig) {
    let dbtx = db.begin_write();

    assert!(
        dbtx.insert(&ServerConfigTable, &(), cfg).is_none(),
        "Server config already present in database"
    );

    dbtx.commit();
}
