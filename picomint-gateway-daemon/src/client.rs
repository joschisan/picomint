use std::net::SocketAddr;

use iroh::Endpoint;
use iroh::endpoint::presets::N0;
use iroh_mdns_address_lookup::MdnsAddressLookup;
use picomint_client::Mnemonic;
use picomint_core::secret::Secret;
use picomint_sqlite::{Database, DbRead};

use crate::db::RootEntropyTable;

/// Initialize a fresh gateway identity, persisting the BIP39 root entropy as
/// the sole root secret. The iroh secret key is derived from the same
/// entropy, so the daemon's `GatewayPk` is reproducible from this row alone.
pub async fn init(
    db: &Database,
    mnemonic: Mnemonic,
    api_addr: SocketAddr,
) -> anyhow::Result<(Endpoint, Mnemonic)> {
    let dbtx = db.begin_write();

    assert!(
        dbtx.insert(&RootEntropyTable, &(), &mnemonic.to_entropy())
            .is_none()
    );

    dbtx.commit();

    let entropy = mnemonic.to_entropy();

    let endpoint = bind_endpoint(&entropy, api_addr).await?;

    Ok((endpoint, mnemonic))
}

/// Try to load an existing gateway identity from the database.
pub async fn try_load(
    db: &Database,
    api_addr: SocketAddr,
) -> anyhow::Result<Option<(Endpoint, Mnemonic)>> {
    let Some(entropy) = db.begin_read().get(&RootEntropyTable, &()) else {
        return Ok(None);
    };

    let mnemonic = Mnemonic::from_entropy(&entropy)
        .map_err(|e| anyhow::anyhow!("Invalid stored entropy: {e}"))?;

    let endpoint = bind_endpoint(&entropy, api_addr).await?;

    Ok(Some((endpoint, mnemonic)))
}

async fn bind_endpoint(entropy: &Vec<u8>, api_addr: SocketAddr) -> anyhow::Result<Endpoint> {
    let iroh_sk = Secret::new_root(entropy).to_iroh_secret_key();

    let endpoint = Endpoint::builder(N0)
        .secret_key(iroh_sk)
        .alpns(vec![picomint_rpc::ALPN.to_vec()])
        .bind_addr(api_addr)?
        .address_lookup(MdnsAddressLookup::builder())
        .bind()
        .await?;

    Ok(endpoint)
}
