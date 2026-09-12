use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Json, State};
use axum::http::StatusCode;
use axum::routing::post;
use picomint_cli_server::{CliError, serve};
use picomint_node_cli_core::{
    BitcoinConnectionResponse, ConsensusPhase, DkgPhase, NodeInfo, NodeStatus, ROUTE_SETUP_ADD,
    ROUTE_SETUP_CONFIRM, ROUTE_SETUP_INIT, ROUTE_SETUP_RESET, ROUTE_SETUP_RESTORE, ROUTE_STATUS,
    SetupAddRequest, SetupAddResponse, SetupInitRequest, SetupInitResponse, SetupPhase,
};
use picomint_redb::{Database, DbRead};

use crate::config::NodeConfig;
use crate::config::db::DkgParamsTable;
use crate::config::setup::SetupApi;
use crate::consensus::api::ConsensusApi;
use crate::consensus::db::consensus_version;
use crate::consensus::{lightning, onchain};
use crate::p2p::{P2PConnectionStatus, Transport};

/// Setup CLI server — runs during the setup phase and is torn down when DKG starts. Binds a Unix socket at
/// `{data_dir}/{CLI_SOCKET_FILENAME}`; a stale socket from a previous
/// (crashed) run is unlinked before we bind.
pub async fn run_cli(data_dir: PathBuf, setup_api: Arc<SetupApi>) {
    let router = Router::new()
        .route(ROUTE_STATUS, post(setup_phase))
        .route(ROUTE_SETUP_INIT, post(setup_init))
        .route(ROUTE_SETUP_ADD, post(setup_add))
        .route(ROUTE_SETUP_RESET, post(setup_reset))
        .route(ROUTE_SETUP_CONFIRM, post(setup_confirm))
        .route(ROUTE_SETUP_RESTORE, post(setup_restore))
        .fallback(wrong_phase("setup"))
        .with_state(setup_api);

    serve(&data_dir, router).await;
}

/// DKG-phase CLI server — answers `status` with the setup code while key
/// generation runs, so an operator polling the socket can tell a node in
/// DKG from one that is down. Every other route names the phase.
pub async fn run_dkg_cli(data_dir: PathBuf, db: Database) {
    let router = Router::new()
        .route(ROUTE_STATUS, post(dkg_phase))
        .fallback(wrong_phase("dkg"))
        .with_state(db);

    serve(&data_dir, router).await;
}

/// Every route is served by exactly one phase, so a miss on the socket
/// means the operator's mental model of the node is stale, not that the
/// route does not exist.
fn wrong_phase(phase: &'static str) -> impl Fn() -> std::future::Ready<CliError> + Clone {
    move || {
        std::future::ready(CliError {
            code: StatusCode::NOT_FOUND,
            error: format!(
                "The node is in the {phase} phase and does not serve this route; run `status`"
            ),
        })
    }
}

/// Build the consensus-phase CLI router that exposes the mint endpoints
/// (invite, config, expiry, status probes) plus the
/// lightning/onchain module-admin routes.
pub fn router(api: Arc<ConsensusApi>) -> Router {
    use picomint_core::expiry::ExpiryStatus;
    use picomint_node_cli_core::{
        ExpirySetRequest, HistoryResponse, INVITE_EXPIRY_DAYS_LIMIT, InviteRequest, InviteResponse,
        LightningGatewayAddRequest, LightningGatewayInfo, LightningGatewayListResponse,
        LightningGatewayRemoveRequest, OnchainStatusResponse, PendingResponse, ROUTE_BACKUP,
        ROUTE_EXPIRY_CLEAR, ROUTE_EXPIRY_SET, ROUTE_EXPIRY_STATUS, ROUTE_GATEWAY_ADD,
        ROUTE_GATEWAY_LIST, ROUTE_GATEWAY_REMOVE, ROUTE_INVITE, ROUTE_ONCHAIN_HISTORY,
        ROUTE_ONCHAIN_PENDING, ROUTE_ONCHAIN_STATUS, ROUTE_ONCHAIN_SWEEP, SweepResponse,
    };

    async fn backup(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
    ) -> Result<Json<NodeConfig>, CliError> {
        Ok(Json(api.server.cfg.clone()))
    }

    async fn invite(
        State(api): State<Arc<ConsensusApi>>,
        Json(req): Json<InviteRequest>,
    ) -> Result<Json<InviteResponse>, CliError> {
        if api.block_height() == 0 {
            return Err(CliError {
                code: StatusCode::SERVICE_UNAVAILABLE,
                error: "Invite codes will be available once the mint has reached consensus on a block height".to_string(),
            });
        }

        if req.expiry_days > INVITE_EXPIRY_DAYS_LIMIT {
            return Err(CliError {
                code: StatusCode::BAD_REQUEST,
                error: format!("Expiration must be at most {INVITE_EXPIRY_DAYS_LIMIT} days"),
            });
        }

        Ok(Json(InviteResponse {
            invite: api.create_invite_code(req.expiry_days, req.user_limit).0,
        }))
    }

    async fn onchain_status(
        State(api): State<Arc<ConsensusApi>>,
    ) -> Result<Json<OnchainStatusResponse>, CliError> {
        // One read snapshot, so every value reflects the same database state.
        let dbtx = api.server.db.begin_read();

        let mint_utxo = onchain::mint_utxo(&dbtx);

        Ok(Json(OnchainStatusResponse {
            total_value_sat: mint_utxo.as_ref().map_or(0, |utxo| utxo.value.to_sat()),
            tx_tip: mint_utxo.map(|utxo| utxo.outpoint.txid),
            tx_count: onchain::total_txs(&dbtx),
            feerate_sat_per_vb: onchain::consensus_feerate(&api.server, &dbtx).map(|f| f / 1000),
        }))
    }

    async fn onchain_sweep(
        State(api): State<Arc<ConsensusApi>>,
    ) -> Result<Json<SweepResponse>, CliError> {
        let code =
            onchain::sweep_secret(&api.server, &api.server.db.begin_read()).ok_or(CliError {
                code: StatusCode::SERVICE_UNAVAILABLE,
                error: "The mint wallet has not received funds yet, so there is nothing to sweep"
                    .to_string(),
            })?;

        Ok(Json(SweepResponse {
            secret: picomint_base32::encode(&code),
        }))
    }

    async fn onchain_pending(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
    ) -> Result<Json<PendingResponse>, CliError> {
        Ok(Json(PendingResponse {
            txs: onchain::pending_tx_chain(&api.server.db.begin_read()),
        }))
    }

    async fn onchain_history(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
    ) -> Result<Json<HistoryResponse>, CliError> {
        Ok(Json(HistoryResponse {
            txs: onchain::tx_chain(&api.server.db.begin_read()),
        }))
    }

    async fn lightning_gateway_add(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
        Json(payload): Json<LightningGatewayAddRequest>,
    ) -> Result<Json<bool>, CliError> {
        Ok(Json(lightning::add_gateway(
            &api.server,
            payload.pk,
            payload.name,
        )))
    }

    async fn lightning_gateway_remove(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
        Json(payload): Json<LightningGatewayRemoveRequest>,
    ) -> Result<Json<bool>, CliError> {
        Ok(Json(lightning::remove_gateway(&api.server, payload.pk)))
    }

    async fn lightning_gateway_list(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
    ) -> Result<Json<LightningGatewayListResponse>, CliError> {
        Ok(Json(LightningGatewayListResponse {
            gateways: lightning::gateways(&api.server.db.begin_read())
                .into_iter()
                .map(|(pk, name)| LightningGatewayInfo { pk, name })
                .collect(),
        }))
    }

    async fn expiry_set(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
        Json(payload): Json<ExpirySetRequest>,
    ) -> Result<Json<()>, CliError> {
        api.set_expiry_status(Some(ExpiryStatus {
            timestamp: payload.timestamp,
            successor: payload.successor,
        }));
        Ok(Json(()))
    }

    async fn expiry_clear(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
    ) -> Result<Json<()>, CliError> {
        api.set_expiry_status(None);
        Ok(Json(()))
    }

    async fn expiry_status(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
    ) -> Result<Json<Option<ExpiryStatus>>, CliError> {
        Ok(Json(api.expiry_status()))
    }

    Router::new()
        .route(ROUTE_STATUS, post(consensus_phase))
        .route(ROUTE_INVITE, post(invite))
        .route(ROUTE_BACKUP, post(backup))
        .route(ROUTE_ONCHAIN_STATUS, post(onchain_status))
        .route(ROUTE_ONCHAIN_PENDING, post(onchain_pending))
        .route(ROUTE_ONCHAIN_HISTORY, post(onchain_history))
        .route(ROUTE_ONCHAIN_SWEEP, post(onchain_sweep))
        .route(ROUTE_GATEWAY_ADD, post(lightning_gateway_add))
        .route(ROUTE_GATEWAY_REMOVE, post(lightning_gateway_remove))
        .route(ROUTE_GATEWAY_LIST, post(lightning_gateway_list))
        .route(ROUTE_EXPIRY_SET, post(expiry_set))
        .route(ROUTE_EXPIRY_CLEAR, post(expiry_clear))
        .route(ROUTE_EXPIRY_STATUS, post(expiry_status))
        .fallback(wrong_phase("consensus"))
        .with_state(api)
}

fn node_infos(api: &ConsensusApi) -> Vec<NodeInfo> {
    api.p2p_status_receivers
        .iter()
        .map(|(node, receiver)| {
            let path = match receiver.borrow().clone() {
                P2PConnectionStatus::Connected(path) => Some(path),
                P2PConnectionStatus::Disconnected => None,
            };

            NodeInfo {
                id: *node,
                name: api
                    .server
                    .cfg
                    .consensus
                    .nodes
                    .get(node)
                    .expect("every node is in the consensus config")
                    .name
                    .clone(),
                connected: path.is_some(),
                transport: path.as_ref().map(|path| match path.transport {
                    Transport::Direct => "direct".to_string(),
                    Transport::Relay => "relay".to_string(),
                }),
                remote_addr: path.as_ref().map(|path| path.remote_addr.clone()),
                rtt_ms: path.map(|path| path.rtt.as_millis() as u64),
            }
        })
        .collect()
}

fn bitcoin_status(api: &ConsensusApi) -> Option<BitcoinConnectionResponse> {
    api.server
        .btc_rpc
        .status()
        .map(|status| BitcoinConnectionResponse {
            network: status.network.to_string(),
            block_height: status.block_height,
            fee_rate_sat_per_vb: status.fee_rate.map(|fee_rate| fee_rate / 1000),
            sync_progress: status.sync_progress,
        })
}

async fn consensus_phase(
    State(api): State<Arc<ConsensusApi>>,
) -> Result<Json<NodeStatus>, CliError> {
    let cfg = &api.server.cfg;

    // One read snapshot, so every value reflects the same database state.
    let dbtx = api.server.db.begin_read();

    let phase = ConsensusPhase {
        mint_name: cfg.consensus.name.clone(),
        mint_id: cfg.consensus.calculate_mint_id(),
        network: cfg.consensus.network.to_string(),
        node_id: cfg.private.identity,
        node_name: cfg
            .consensus
            .nodes
            .get(&cfg.private.identity)
            .expect("our node is in the consensus config")
            .name
            .clone(),
        consensus_version: consensus_version(&api.server, &dbtx),
        session_count: api.session_count(),
        block_height: api.block_height(),
        nodes: node_infos(&api),
        bitcoin: bitcoin_status(&api),
    };

    Ok(Json(NodeStatus::Consensus(Box::new(phase))))
}

async fn dkg_phase(State(db): State<Database>) -> Result<Json<NodeStatus>, CliError> {
    // `store_node_config` clears the table moments before this server is
    // aborted, so a request can land after DKG has completed.
    let params = db.begin_read().get(&DkgParamsTable, &()).ok_or(CliError {
        code: StatusCode::SERVICE_UNAVAILABLE,
        error: "DKG has just completed; the node is starting consensus".to_string(),
    })?;

    let phase = DkgPhase {
        setup_code: picomint_base32::encode(
            params
                .nodes
                .get(&params.identity)
                .expect("our node id is always in the node map"),
        ),
    };

    Ok(Json(NodeStatus::Dkg(phase)))
}

/// Consensus-phase CLI server. Binds a Unix
/// socket at `{data_dir}/{CLI_SOCKET_FILENAME}`; a stale socket from a
/// previous (crashed) run is unlinked before we bind.
pub async fn run(data_dir: PathBuf, router: Router) {
    serve(&data_dir, router).await;
}

// Setup handlers

async fn setup_phase(State(setup_api): State<Arc<SetupApi>>) -> Result<Json<NodeStatus>, CliError> {
    let phase = SetupPhase {
        setup_code: setup_api
            .setup_code()
            .await
            .map(|code| picomint_base32::encode(&code)),
        node_name: setup_api.node_name().await,
        mint_name: setup_api.cfg_mint_name().await,
        mint_size: setup_api.mint_size().await,
        nodes: setup_api.connected_nodes().await,
    };

    Ok(Json(NodeStatus::Setup(phase)))
}

async fn setup_reset(State(setup_api): State<Arc<SetupApi>>) -> Result<Json<()>, CliError> {
    setup_api.reset_setup_codes().await;

    Ok(Json(()))
}

async fn setup_init(
    State(setup_api): State<Arc<SetupApi>>,
    Json(payload): Json<SetupInitRequest>,
) -> Result<Json<SetupInitResponse>, CliError> {
    let setup_code = setup_api
        .init(payload.name, payload.mint_name, payload.mint_size)
        .await
        .map_err(CliError::internal)?;

    Ok(Json(SetupInitResponse { setup_code }))
}

async fn setup_add(
    State(setup_api): State<Arc<SetupApi>>,
    Json(payload): Json<SetupAddRequest>,
) -> Result<Json<SetupAddResponse>, CliError> {
    let name = setup_api
        .add_node_setup_code(payload.setup_code)
        .await
        .map_err(CliError::internal)?;

    Ok(Json(SetupAddResponse { name }))
}

async fn setup_confirm(State(setup_api): State<Arc<SetupApi>>) -> Result<Json<()>, CliError> {
    setup_api.start_dkg().await.map_err(CliError::internal)?;

    Ok(Json(()))
}

async fn setup_restore(
    State(setup_api): State<Arc<SetupApi>>,
    Json(cfg): Json<NodeConfig>,
) -> Result<Json<()>, CliError> {
    setup_api
        .restore_config(cfg)
        .await
        .map_err(CliError::internal)?;

    Ok(Json(()))
}
