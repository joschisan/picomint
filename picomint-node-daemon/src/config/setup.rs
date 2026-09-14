use std::iter::once;
use std::sync::Arc;

use iroh::SecretKey;
use picomint_bitcoind::BitcoindClient;
use picomint_core::NodeId;
use picomint_core::config::NodeSetupCode;
use picomint_encoding::{Decodable, Encodable};
use picomint_node_cli_core::{SetupAddError, SetupConfirmError, SetupInitError, SetupRestoreError};
use picomint_redb::{Database, DbRead};
use tokio::sync::Mutex;
use tokio::sync::mpsc::Sender;

use crate::config::db::{DkgParamsTable, InitParamsTable, store_node_config};
use crate::config::{DkgParams, NodeConfig, SetupResult};

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
    /// Bitcoin backend; `confirm` reads the mint's network off it
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
    ) -> Result<NodeSetupCode, SetupInitError> {
        if let Some(existing_init_params) = self.state.lock().await.init_params.clone()
            && existing_init_params.name == name
            && existing_init_params.mint_name == mint_name
            && existing_init_params.mint_size == mint_size
        {
            return Ok(existing_init_params.setup_code());
        }

        if name.is_empty() {
            return Err(SetupInitError::EmptyNodeName);
        }

        if mint_name.as_ref().is_some_and(String::is_empty) {
            return Err(SetupInitError::EmptyMintName);
        }

        if mint_name.is_some() && mint_size.is_none() {
            return Err(SetupInitError::MintSizeMissing);
        }

        if mint_size.is_some_and(|size| size < 4) {
            return Err(SetupInitError::MintSizeTooSmall);
        }

        let mut state = self.state.lock().await;

        if state.init_params.is_some() {
            return Err(SetupInitError::AlreadyInitialized);
        }

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

        Ok(params.setup_code())
    }

    pub async fn add_node_setup_code(&self, info: NodeSetupCode) -> Result<String, SetupAddError> {
        let mut state = self.state.lock().await;

        if state.other_setup_codes.contains(&info) {
            return Ok(info.name.clone());
        }

        let init_params = state
            .init_params
            .clone()
            .ok_or(SetupAddError::NotInitialized)?;

        if info == init_params.setup_code() {
            return Err(SetupAddError::OwnSetupCode);
        }

        if let Some(mint_name) = state
            .other_setup_codes
            .iter()
            .chain(once(&init_params.setup_code()))
            .find_map(|info| info.mint_name.clone())
            && info.mint_name.is_some()
        {
            return Err(SetupAddError::MintNameAlreadySet(mint_name));
        }

        if let Some(mint_size) = state
            .other_setup_codes
            .iter()
            .chain(once(&init_params.setup_code()))
            .find_map(|info| info.mint_size)
            && info.mint_size.is_some()
        {
            return Err(SetupAddError::MintSizeAlreadySet(mint_size));
        }

        state.other_setup_codes.insert(info.clone());

        Ok(info.name)
    }

    pub async fn start_dkg(&self) -> Result<(), SetupConfirmError> {
        let state = self.state.lock().await.clone();

        let init_params = state.init_params.ok_or(SetupConfirmError::NotInitialized)?;

        let our_setup_code = init_params.setup_code();

        let mut setup_codes = state.other_setup_codes;

        setup_codes.insert(our_setup_code.clone());

        if setup_codes.len() < 4 {
            return Err(SetupConfirmError::MintSizeTooSmall);
        }

        if let Some(mint_size) = setup_codes.iter().find_map(|info| info.mint_size)
            && setup_codes.len() != mint_size as usize
        {
            return Err(SetupConfirmError::WrongNodeCount {
                expected: mint_size,
                got: setup_codes.len(),
            });
        }

        let mint_name = setup_codes
            .iter()
            .find_map(|info| info.mint_name.clone())
            .ok_or(SetupConfirmError::MintNameMissing)?;

        let our_id = setup_codes
            .iter()
            .position(|info| info == &our_setup_code)
            .expect("We inserted the key above.");

        let network = self
            .bitcoin
            .network()
            .await
            .map_err(|e| SetupConfirmError::Bitcoind(e.to_string()))?;

        if network == bitcoin::Network::Bitcoin {
            return Err(SetupConfirmError::Mainnet);
        }

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
            .map_err(|_| SetupConfirmError::Completed)?;

        Ok(())
    }

    pub async fn restore_config(&self, cfg: NodeConfig) -> Result<(), SetupRestoreError> {
        super::validate_config(&cfg)
            .map_err(|e| SetupRestoreError::InvalidConfig(e.to_string()))?;

        store_node_config(&self.db, &cfg).await;

        self.sender
            .send(SetupResult::Restored(Box::new(cfg)))
            .await
            .map_err(|_| SetupRestoreError::Completed)?;

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
