use std::net::SocketAddr;
use std::sync::Arc;

use bitcoin::Network;
use iroh::Endpoint;
use iroh::endpoint::presets::N0;
use iroh_mdns_address_lookup::MdnsAddressLookup;
use picomint_client::{Client, Mnemonic};
use picomint_core::config::{ConsensusConfig, FederationId};
use picomint_core::invite::InviteCode;
use picomint_core::secret::Secret;
use picomint_redb::Database;

use crate::db::{CLIENT_CONFIG, ROOT_ENTROPY};

#[derive(Clone)]
pub struct GatewayClientFactory {
    db: Database,
    mnemonic: Mnemonic,
    endpoint: Endpoint,
    network: Network,
}

impl GatewayClientFactory {
    /// Initialize a new factory, persisting the BIP39 root entropy as the
    /// sole root secret. The iroh secret key is derived from the same entropy,
    /// so the daemon's `GatewayPk` is reproducible from this row alone.
    pub async fn init(
        db: Database,
        mnemonic: Mnemonic,
        network: Network,
        api_addr: SocketAddr,
    ) -> anyhow::Result<Self> {
        let dbtx = db.begin_write();

        assert!(
            dbtx.insert(&ROOT_ENTROPY, &(), &mnemonic.to_entropy())
                .is_none()
        );

        dbtx.commit();

        let iroh_sk = Secret::new_root(&mnemonic.to_entropy()).to_iroh_secret_key();

        let endpoint = Endpoint::builder(N0)
            .secret_key(iroh_sk)
            .alpns(vec![picomint_rpc::ALPN.to_vec()])
            .bind_addr(api_addr)?
            .address_lookup(MdnsAddressLookup::builder())
            .bind()
            .await?;

        Ok(Self {
            endpoint,
            db,
            mnemonic,
            network,
        })
    }

    /// Try to load an existing factory from the database.
    pub async fn try_load(
        db: Database,
        network: Network,
        api_addr: SocketAddr,
    ) -> anyhow::Result<Option<Self>> {
        let Some(entropy) = db.begin_read().as_ref().get(&ROOT_ENTROPY, &()) else {
            return Ok(None);
        };

        let mnemonic = Mnemonic::from_entropy(&entropy)
            .map_err(|e| anyhow::anyhow!("Invalid stored entropy: {e}"))?;

        let iroh_sk = Secret::new_root(&entropy).to_iroh_secret_key();

        let endpoint = Endpoint::builder(N0)
            .secret_key(iroh_sk)
            .alpns(vec![picomint_rpc::ALPN.to_vec()])
            .bind_addr(api_addr)?
            .address_lookup(MdnsAddressLookup::builder())
            .bind()
            .await?;

        Ok(Some(Self {
            endpoint,
            db,
            mnemonic,
            network,
        }))
    }

    /// The iroh endpoint owned by this factory. Re-used as the gateway
    /// daemon's accept-side endpoint for the public API.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub fn mnemonic(&self) -> &Mnemonic {
        &self.mnemonic
    }

    fn client_database(&self, federation_id: FederationId) -> Database {
        self.db.isolate(federation_id)
    }

    async fn read_config(&self, federation_id: &FederationId) -> Option<ConsensusConfig> {
        self.db
            .begin_read()
            .as_ref()
            .get(&CLIENT_CONFIG, federation_id)
    }

    /// Join a federation for the first time. Errors if a config for this
    /// federation is already persisted — use [`Self::load`] in that case.
    pub async fn join(&self, invite: &InviteCode) -> anyhow::Result<Arc<picomint_client::Client>> {
        let config = picomint_client::download(&self.endpoint, invite).await?;

        if config.network != self.network {
            anyhow::bail!("Unsupported network {}", config.network);
        }

        let dbtx = self.db.begin_write();

        if dbtx
            .as_ref()
            .insert(&CLIENT_CONFIG, &config.calculate_federation_id(), &config)
            .is_some()
        {
            anyhow::bail!("Federation is already joined");
        }

        dbtx.commit();

        self.open(config).await
    }

    /// Open the client for a federation whose config is already persisted.
    /// Returns `None` if no config is stored for `federation_id`.
    pub async fn load(
        &self,
        federation_id: &FederationId,
    ) -> anyhow::Result<Option<Arc<picomint_client::Client>>> {
        match self.read_config(federation_id).await {
            Some(config) => self.open(config).await.map(Some),
            None => Ok(None),
        }
    }

    async fn open(&self, config: ConsensusConfig) -> anyhow::Result<Arc<picomint_client::Client>> {
        Client::new_gateway(
            self.endpoint.clone(),
            self.client_database(config.calculate_federation_id()),
            &self.mnemonic,
            config,
        )
        .await
        .map_err(|e| anyhow::anyhow!("Client open error: {e}"))
    }

    /// List all federation ids the gateway has joined.
    pub async fn list_federations(&self) -> Vec<FederationId> {
        self.db
            .begin_read()
            .as_ref()
            .iter(&CLIENT_CONFIG, |r| r.map(|(id, _)| id).collect())
    }
}
