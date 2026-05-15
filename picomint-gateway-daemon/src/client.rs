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
use picomint_eventlog::EventLogger;
use picomint_redb::Database;

use crate::db::{ClientConfigTable, RootEntropyTable};

#[derive(Clone)]
pub struct GatewayClientFactory {
    db: Database,
    logger: EventLogger,
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
        logger: EventLogger,
        mnemonic: Mnemonic,
        network: Network,
        api_addr: SocketAddr,
    ) -> anyhow::Result<Self> {
        let dbtx = db.begin_write();

        assert!(
            dbtx.insert(&RootEntropyTable, &(), &mnemonic.to_entropy())
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
            logger,
            mnemonic,
            network,
        })
    }

    /// Try to load an existing factory from the database.
    pub async fn try_load(
        db: Database,
        logger: EventLogger,
        network: Network,
        api_addr: SocketAddr,
    ) -> anyhow::Result<Option<Self>> {
        let Some(entropy) = db.begin_read().get(&RootEntropyTable, &()) else {
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
            logger,
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

    fn read_config(&self, federation: &FederationId) -> Option<ConsensusConfig> {
        self.db.begin_read().get(&ClientConfigTable, federation)
    }

    /// Download and persist the consensus config for a federation. The
    /// `Client` itself is brought up lazily on first use via
    /// [`AppState::select_client`]. Errors if a config for this federation is
    /// already persisted.
    pub async fn join(&self, invite: &InviteCode) -> anyhow::Result<()> {
        let config = picomint_client::download(&self.endpoint, invite).await?;

        if config.network != self.network {
            anyhow::bail!("Unsupported network {}", config.network);
        }

        let dbtx = self.db.begin_write();

        if dbtx
            .insert(
                &ClientConfigTable,
                &config.calculate_federation_id(),
                &config,
            )
            .is_some()
        {
            anyhow::bail!("Federation is already joined");
        }

        dbtx.commit();

        Ok(())
    }

    /// Open the client for a federation whose config is already persisted.
    /// Returns `None` if no config is stored for `federation`.
    pub fn load(
        &self,
        federation: &FederationId,
    ) -> anyhow::Result<Option<Arc<picomint_client::Client>>> {
        match self.read_config(federation) {
            Some(config) => self.open(config).map(Some),
            None => Ok(None),
        }
    }

    fn open(&self, config: ConsensusConfig) -> anyhow::Result<Arc<picomint_client::Client>> {
        Client::new_gateway(
            self.endpoint.clone(),
            self.db.clone(),
            self.logger.clone(),
            &self.mnemonic,
            config,
        )
        .map_err(|e| anyhow::anyhow!("Client open error: {e}"))
    }
}
