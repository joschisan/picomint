use std::iter::once;
use std::sync::Arc;

use anyhow::{Context, ensure};
use iroh::SecretKey;
use picomint_core::NodeId;
use picomint_encoding::{Decodable, Encodable};
use picomint_redb::{Database, DbRead};
use serde::Serialize;
use tokio::sync::Mutex;
use tokio::sync::mpsc::Sender;

use crate::bitcoind::BitcoindClient;
use crate::config::db::{DkgParamsTable, InitParamsTable, store_node_config};
use crate::config::{DkgParams, NodeConfig, SetupResult};

/// The setup code shared between nodes during setup.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Encodable, Decodable, Serialize)]
pub struct NodeSetupCode {
    /// Name of the node
    pub name: String,
    /// Public key of the node's single iroh endpoint (serves both p2p and
    /// client-API traffic, demuxed by node-id on accept).
    pub pk: iroh_base::PublicKey,
    /// Mint name set by the leader
    pub mint_name: Option<String>,
    /// Total number of nodes (including the one who sets this), set by the
    /// leader
    pub mint_size: Option<u8>,
}

/// In-memory state of the setup phase.
#[derive(Debug, Clone, Default)]
pub struct SetupState {
    /// This node's own parameters; `None` until `init` has run
    init_params: Option<InitParams>,
    /// Setup codes received from other nodes
    other_setup_codes: std::collections::BTreeSet<NodeSetupCode>,
}

#[derive(Clone, Debug, Encodable, Decodable)]
/// This node's own setup parameters, created by `init` and persisted so a
/// daemon restart mid-setup keeps the iroh identity. Never leaves the node —
/// only the derived [`NodeSetupCode`] is shared.
pub struct InitParams {
    /// Secret key for our single iroh endpoint (p2p + api)
    iroh_sk: iroh::SecretKey,
    /// Name of the node
    name: String,
    /// Mint name set by the leader
    mint_name: Option<String>,
    /// Total number of nodes (including the one who sets this), set by the
    /// leader
    mint_size: Option<u8>,
}

impl InitParams {
    pub fn setup_code(&self) -> NodeSetupCode {
        NodeSetupCode {
            name: self.name.clone(),
            pk: self.iroh_sk.public(),
            mint_name: self.mint_name.clone(),
            mint_size: self.mint_size,
        }
    }
}

/// Serves the setup API endpoints
#[derive(Clone)]
pub struct SetupApi {
    /// Bitcoin backend; `start_dkg` reads the mint's network off it
    /// instead of trusting a locally-configured value.
    bitcoin: Arc<BitcoindClient>,
    /// In-memory state machine, mirroring the on-disk setup tables.
    state: Arc<Mutex<SetupState>>,
    /// Signals the setup loop with either DKG params or a restored config
    sender: Sender<SetupResult>,
    /// Backing store; setup mutations write through here so a daemon restart
    /// mid-setup keeps the iroh identity and the already-collected node
    /// codes.
    db: Database,
}

impl SetupApi {
    pub fn new(bitcoin: Arc<BitcoindClient>, sender: Sender<SetupResult>, db: Database) -> Self {
        let state = SetupState {
            init_params: db.begin_read().get(&InitParamsTable, &()),
            other_setup_codes: std::collections::BTreeSet::new(),
        };

        Self {
            bitcoin,
            state: Arc::new(Mutex::new(state)),
            sender,
            db,
        }
    }

    pub async fn setup_code(&self) -> Option<NodeSetupCode> {
        self.state
            .lock()
            .await
            .init_params
            .as_ref()
            .map(InitParams::setup_code)
    }

    pub async fn node_name(&self) -> Option<String> {
        self.state
            .lock()
            .await
            .init_params
            .as_ref()
            .map(|params| params.name.clone())
    }

    pub async fn connected_nodes(&self) -> Vec<String> {
        self.state
            .lock()
            .await
            .other_setup_codes
            .clone()
            .into_iter()
            .map(|info| info.name)
            .collect()
    }

    pub async fn reset_setup_codes(&self) {
        self.state.lock().await.other_setup_codes.clear();
    }

    pub async fn init(
        &self,
        name: String,
        mint_name: Option<String>,
        mint_size: Option<u8>,
    ) -> anyhow::Result<String> {
        if let Some(existing_init_params) = self.state.lock().await.init_params.clone()
            && existing_init_params.name == name
            && existing_init_params.mint_name == mint_name
            && existing_init_params.mint_size == mint_size
        {
            return Ok(picomint_base32::encode(&existing_init_params.setup_code()));
        }

        ensure!(!name.is_empty(), "The node name is empty");

        if let Some(mint_name) = mint_name.as_ref() {
            ensure!(!mint_name.is_empty(), "The mint name is empty");
        }

        if mint_name.is_some() {
            ensure!(mint_size.is_some(), "The leader must set the mint size");
        }

        if let Some(size) = mint_size {
            ensure!(size >= 4, "Mint size must be at least 4");
        }

        let mut state = self.state.lock().await;

        ensure!(
            state.init_params.is_none(),
            "The node has already been initialized"
        );

        let iroh_sk = SecretKey::from_bytes(&rand::random());

        let params = InitParams {
            iroh_sk,
            name,
            mint_name,
            mint_size,
        };

        let dbtx = self.db.begin_write();

        dbtx.insert(&InitParamsTable, &(), &params);

        dbtx.commit();

        state.init_params = Some(params.clone());

        Ok(picomint_base32::encode(&params.setup_code()))
    }

    pub async fn add_node_setup_code(&self, info: String) -> anyhow::Result<String> {
        let info = picomint_base32::decode(&info)?;

        let mut state = self.state.lock().await;

        if state.other_setup_codes.contains(&info) {
            return Ok(info.name.clone());
        }

        let init_params = state
            .init_params
            .clone()
            .context("The node has not been initialized yet")?;

        ensure!(
            info != init_params.setup_code(),
            "You cannot add your own setup code"
        );

        if let Some(mint_name) = state
            .other_setup_codes
            .iter()
            .chain(once(&init_params.setup_code()))
            .find_map(|info| info.mint_name.clone())
        {
            ensure!(
                info.mint_name.is_none(),
                "Mint name has already been set to {mint_name}"
            );
        }

        if let Some(mint_size) = state
            .other_setup_codes
            .iter()
            .chain(once(&init_params.setup_code()))
            .find_map(|info| info.mint_size)
        {
            ensure!(
                info.mint_size.is_none(),
                "Mint size has already been set to {mint_size}"
            );
        }

        state.other_setup_codes.insert(info.clone());

        Ok(info.name)
    }

    pub async fn start_dkg(&self) -> anyhow::Result<()> {
        let state = self.state.lock().await.clone();

        let init_params = state
            .init_params
            .context("The node has not been initialized yet")?;

        let our_setup_code = init_params.setup_code();

        let mut setup_codes = state.other_setup_codes;

        setup_codes.insert(our_setup_code.clone());

        ensure!(setup_codes.len() >= 4, "Mint size must be at least 4");

        if let Some(mint_size) = setup_codes.iter().find_map(|info| info.mint_size) {
            ensure!(
                setup_codes.len() == mint_size as usize,
                "Expected {mint_size} nodes but got {}",
                setup_codes.len()
            );
        }

        let mint_name = setup_codes
            .iter()
            .find_map(|info| info.mint_name.clone())
            .context("We need one node to configure the mints name")?;

        let our_id = setup_codes
            .iter()
            .position(|info| info == &our_setup_code)
            .expect("We inserted the key above.");

        let network = self
            .bitcoin
            .network()
            .await
            .context("Failed to determine the network from the bitcoin backend")?;

        let params = DkgParams {
            identity: NodeId::from(our_id as u8),
            iroh_sk: init_params.iroh_sk,
            nodes: (0..)
                .map(|i| NodeId::from(i as u8))
                .zip(setup_codes)
                .collect(),
            name: mint_name,
            network,
        };

        // Atomically transition out of the code-exchange phase: drop the
        // `InitParams` (its iroh secret key is now inside `params`) and
        // persist `DkgParams` so a daemon restart auto-resumes DKG
        // without operator interaction.
        let dbtx = self.db.begin_write();

        dbtx.clear_table(&InitParamsTable);

        dbtx.insert(&DkgParamsTable, &(), &params);

        dbtx.commit();

        self.sender
            .send(SetupResult::Dkg(Box::new(params)))
            .await
            .context("Failed to send DKG params")?;

        Ok(())
    }

    pub async fn restore_config(&self, cfg: NodeConfig) -> anyhow::Result<()> {
        cfg.validate_config()
            .context("Restored config failed validation")?;

        store_node_config(&self.db, &cfg).await;

        self.sender
            .send(SetupResult::Restored(Box::new(cfg)))
            .await
            .context("Failed to send restored config")?;

        Ok(())
    }

    pub async fn mint_size(&self) -> Option<u8> {
        let state = self.state.lock().await;
        let our_setup_code = state.init_params.as_ref().map(InitParams::setup_code);
        state
            .other_setup_codes
            .iter()
            .chain(our_setup_code.iter())
            .find_map(|info| info.mint_size)
    }

    pub async fn cfg_mint_name(&self) -> Option<String> {
        let state = self.state.lock().await;
        let our_setup_code = state.init_params.as_ref().map(InitParams::setup_code);
        state
            .other_setup_codes
            .iter()
            .chain(our_setup_code.iter())
            .find_map(|info| info.mint_name.clone())
    }
}
