use picomint_redb::table;
use picomint_redb::{Database, DbRead};

use crate::config::setup::InitParams;
use crate::config::{DkgParams, NodeConfig};

table!(
    NodeConfigTable,
    () => NodeConfig,
    "node-config",
);

table!(
    InitParamsTable,
    () => InitParams,
    "setup-init-params",
);

table!(
    DkgParamsTable,
    () => DkgParams,
    "setup-dkg-params",
);

pub async fn load_node_config(db: &Database) -> Option<NodeConfig> {
    db.begin_read().get(&NodeConfigTable, &())
}

/// Persist the finalized `NodeConfig` and drop any leftover setup-phase
/// state in the same write tx — once consensus has a config, the
/// `InitParams` / `DkgParams` entries are dead weight.
pub async fn store_node_config(db: &Database, cfg: &NodeConfig) {
    let dbtx = db.begin_write();

    assert!(
        dbtx.insert(&NodeConfigTable, &(), cfg).is_none(),
        "Node config already present in database"
    );

    dbtx.clear_table(&InitParamsTable);
    dbtx.clear_table(&DkgParamsTable);

    dbtx.commit();
}
