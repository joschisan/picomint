use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Json, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use picomint_guardian_cli_core::{
    CLI_SOCKET_FILENAME, ROUTE_SETUP_ADD_PEER, ROUTE_SETUP_RESTORE, ROUTE_SETUP_SET_LOCAL_PARAMS,
    ROUTE_SETUP_START_DKG, ROUTE_SETUP_STATUS, SetupAddPeerRequest, SetupAddPeerResponse,
    SetupSetLocalParamsRequest, SetupSetLocalParamsResponse, SetupStatus,
};
use tokio::net::UnixListener;

use crate::config::ServerConfig;
use crate::config::setup::SetupApi;
use crate::consensus::{ln, wallet};

#[derive(Debug)]
pub struct CliError {
    pub code: StatusCode,
    pub error: String,
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error)
    }
}

impl std::error::Error for CliError {}

impl CliError {
    pub fn internal(error: impl std::fmt::Display) -> Self {
        Self {
            code: StatusCode::INTERNAL_SERVER_ERROR,
            error: error.to_string(),
        }
    }
}

impl IntoResponse for CliError {
    fn into_response(self) -> axum::response::Response {
        (self.code, self.error).into_response()
    }
}

impl From<anyhow::Error> for CliError {
    fn from(e: anyhow::Error) -> Self {
        Self::internal(e)
    }
}

/// Setup CLI server — runs during DKG phase. Binds a Unix socket at
/// `{data_dir}/{CLI_SOCKET_FILENAME}`; a stale socket from a previous
/// (crashed) run is unlinked before we bind.
pub async fn run_cli(data_dir: PathBuf, setup_api: Arc<SetupApi>) {
    let socket_path = data_dir.join(CLI_SOCKET_FILENAME);
    std::fs::remove_file(&socket_path).ok();

    let listener = UnixListener::bind(&socket_path).expect("Failed to bind CLI server");

    let router = Router::new()
        .route(ROUTE_SETUP_STATUS, post(setup_status))
        .route(ROUTE_SETUP_SET_LOCAL_PARAMS, post(setup_set_local_params))
        .route(ROUTE_SETUP_ADD_PEER, post(setup_add_peer))
        .route(ROUTE_SETUP_START_DKG, post(setup_start_dkg))
        .route(ROUTE_SETUP_RESTORE, post(setup_restore))
        .with_state(setup_api)
        .into_make_service();

    axum::serve(listener, router)
        .await
        .expect("CLI admin server failed");
}

/// Build the Dashboard-phase CLI router that exposes read-only federation
/// endpoints (audit, invite) plus the LN/wallet module-admin routes.
pub fn router(api: Arc<crate::consensus::api::ConsensusApi>) -> Router {
    use crate::p2p::{P2PConnectionStatus, Transport};
    use axum::Json;
    use axum::routing::post;
    use picomint_core::expiry::ExpiryStatus;
    use picomint_guardian_cli_core::{
        AuditResponse, BitcoinConnectionResponse, BlockCountResponse, ExpirySetRequest,
        INVITE_EXPIRY_DAYS_LIMIT, InviteRequest, InviteResponse, LnGatewayAddRequest,
        LnGatewayInfo, LnGatewayListResponse, LnGatewayRemoveRequest, P2pResponse, PeerInfo,
        PendingTxsResponse, ROUTE_AUDIT, ROUTE_BITCOIN_CONNECTION, ROUTE_BLOCK_COUNT, ROUTE_CONFIG,
        ROUTE_EXPIRY_CLEAR, ROUTE_EXPIRY_SET, ROUTE_EXPIRY_STATUS, ROUTE_INVITE,
        ROUTE_MODULE_LN_GATEWAY_ADD, ROUTE_MODULE_LN_GATEWAY_LIST, ROUTE_MODULE_LN_GATEWAY_REMOVE,
        ROUTE_MODULE_WALLET_FEERATE, ROUTE_MODULE_WALLET_PENDING_TXS,
        ROUTE_MODULE_WALLET_TOTAL_VALUE, ROUTE_MODULE_WALLET_TXS, ROUTE_P2P, ROUTE_SESSION_COUNT,
        TxsResponse, WalletFeerateResponse, WalletTotalValueResponse,
    };

    async fn config(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
    ) -> Result<Json<ServerConfig>, CliError> {
        Ok(Json(api.server.cfg.clone()))
    }

    async fn session_count(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
    ) -> Result<Json<u32>, CliError> {
        Ok(Json(api.session_count()))
    }

    async fn invite(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
        Json(req): Json<InviteRequest>,
    ) -> Result<Json<InviteResponse>, CliError> {
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

    async fn audit(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
    ) -> Result<Json<AuditResponse>, CliError> {
        Ok(Json(AuditResponse {
            audit: api.federation_audit(),
        }))
    }

    async fn wallet_total_value(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
    ) -> Result<Json<WalletTotalValueResponse>, CliError> {
        Ok(Json(WalletTotalValueResponse {
            total_value_sat: wallet::federation_wallet(&api.server.db.begin_read())
                .map(|w| w.value.to_sat()),
        }))
    }

    async fn block_count(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
    ) -> Result<Json<BlockCountResponse>, CliError> {
        Ok(Json(BlockCountResponse {
            block_count: api.block_count(),
        }))
    }

    async fn p2p(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
    ) -> Result<Json<P2pResponse>, CliError> {
        let peers = api
            .p2p_status_receivers
            .iter()
            .map(|(peer, receiver)| {
                let path = match receiver.borrow().clone() {
                    P2PConnectionStatus::Connected(path) => Some(path),
                    P2PConnectionStatus::Disconnected => None,
                };

                PeerInfo {
                    id: *peer,
                    name: api
                        .server
                        .cfg
                        .consensus
                        .peers
                        .get(peer)
                        .expect("every peer is in the consensus config")
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
            .collect();

        Ok(Json(P2pResponse { peers }))
    }

    async fn bitcoin_connection(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
    ) -> Result<Json<BitcoinConnectionResponse>, CliError> {
        let status = api.server.btc_rpc.status().ok_or(CliError {
            code: StatusCode::SERVICE_UNAVAILABLE,
            error: "Not connected to the bitcoin backend yet".to_string(),
        })?;

        Ok(Json(BitcoinConnectionResponse {
            network: status.network.to_string(),
            block_count: status.block_count,
            fee_rate_sat_per_vb: status.fee_rate.map(|fee_rate| fee_rate.sat_per_kvb / 1000),
            sync_progress: status.sync_progress,
        }))
    }

    async fn wallet_feerate(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
    ) -> Result<Json<WalletFeerateResponse>, CliError> {
        Ok(Json(WalletFeerateResponse {
            sat_per_vbyte: wallet::consensus_feerate(&api.server, &api.server.db.begin_read())
                .map(|f| f / 1000),
        }))
    }

    async fn wallet_pending_txs(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
    ) -> Result<Json<PendingTxsResponse>, CliError> {
        Ok(Json(PendingTxsResponse {
            txs: wallet::pending_tx_chain(&api.server.db.begin_read()),
        }))
    }

    async fn wallet_txs(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
    ) -> Result<Json<TxsResponse>, CliError> {
        Ok(Json(TxsResponse {
            txs: wallet::tx_chain(&api.server.db.begin_read()),
        }))
    }

    async fn ln_gateway_add(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
        Json(payload): Json<LnGatewayAddRequest>,
    ) -> Result<Json<bool>, CliError> {
        Ok(Json(ln::add_gateway(&api.server, payload.pk, payload.name)))
    }

    async fn ln_gateway_remove(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
        Json(payload): Json<LnGatewayRemoveRequest>,
    ) -> Result<Json<bool>, CliError> {
        Ok(Json(ln::remove_gateway(&api.server, payload.pk)))
    }

    async fn ln_gateway_list(
        State(api): State<Arc<crate::consensus::api::ConsensusApi>>,
    ) -> Result<Json<LnGatewayListResponse>, CliError> {
        Ok(Json(LnGatewayListResponse {
            gateways: ln::gateways(&api.server.db.begin_read())
                .into_iter()
                .map(|(pk, name)| LnGatewayInfo { pk, name })
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
        .route(ROUTE_INVITE, post(invite))
        .route(ROUTE_AUDIT, post(audit))
        .route(ROUTE_CONFIG, post(config))
        .route(ROUTE_SESSION_COUNT, post(session_count))
        .route(ROUTE_BLOCK_COUNT, post(block_count))
        .route(ROUTE_P2P, post(p2p))
        .route(ROUTE_BITCOIN_CONNECTION, post(bitcoin_connection))
        .route(ROUTE_MODULE_WALLET_TOTAL_VALUE, post(wallet_total_value))
        .route(ROUTE_MODULE_WALLET_FEERATE, post(wallet_feerate))
        .route(ROUTE_MODULE_WALLET_PENDING_TXS, post(wallet_pending_txs))
        .route(ROUTE_MODULE_WALLET_TXS, post(wallet_txs))
        .route(ROUTE_MODULE_LN_GATEWAY_ADD, post(ln_gateway_add))
        .route(ROUTE_MODULE_LN_GATEWAY_REMOVE, post(ln_gateway_remove))
        .route(ROUTE_MODULE_LN_GATEWAY_LIST, post(ln_gateway_list))
        .route(ROUTE_EXPIRY_SET, post(expiry_set))
        .route(ROUTE_EXPIRY_CLEAR, post(expiry_clear))
        .route(ROUTE_EXPIRY_STATUS, post(expiry_status))
        .with_state(api)
}

/// Dashboard CLI server — runs during consensus phase. Binds a Unix
/// socket at `{data_dir}/{CLI_SOCKET_FILENAME}`; a stale socket from a
/// previous (crashed) run is unlinked before we bind.
pub async fn run(data_dir: PathBuf, router: Router) {
    let socket_path = data_dir.join(CLI_SOCKET_FILENAME);

    std::fs::remove_file(&socket_path).ok();

    let listener = UnixListener::bind(&socket_path).expect("Failed to bind module CLI server");

    axum::serve(listener, router.into_make_service())
        .await
        .expect("Module CLI admin server failed");
}

// Setup handlers

async fn setup_status(
    State(setup_api): State<Arc<SetupApi>>,
) -> Result<Json<SetupStatus>, CliError> {
    let status = if setup_api.setup_code().await.is_some() {
        SetupStatus::SharingConnectionCodes
    } else {
        SetupStatus::AwaitingLocalParams
    };
    Ok(Json(status))
}

async fn setup_set_local_params(
    State(setup_api): State<Arc<SetupApi>>,
    Json(payload): Json<SetupSetLocalParamsRequest>,
) -> Result<Json<SetupSetLocalParamsResponse>, CliError> {
    let setup_code = setup_api
        .set_local_parameters(
            payload.name,
            payload.federation_name,
            payload.federation_size,
        )
        .await
        .map_err(CliError::internal)?;

    Ok(Json(SetupSetLocalParamsResponse { setup_code }))
}

async fn setup_add_peer(
    State(setup_api): State<Arc<SetupApi>>,
    Json(payload): Json<SetupAddPeerRequest>,
) -> Result<Json<SetupAddPeerResponse>, CliError> {
    let name = setup_api
        .add_peer_setup_code(payload.setup_code)
        .await
        .map_err(CliError::internal)?;

    Ok(Json(SetupAddPeerResponse { name }))
}

async fn setup_start_dkg(State(setup_api): State<Arc<SetupApi>>) -> Result<Json<()>, CliError> {
    setup_api.start_dkg().await.map_err(CliError::internal)?;

    Ok(Json(()))
}

async fn setup_restore(
    State(setup_api): State<Arc<SetupApi>>,
    Json(cfg): Json<ServerConfig>,
) -> Result<Json<()>, CliError> {
    setup_api
        .restore_config(cfg)
        .await
        .map_err(CliError::internal)?;

    Ok(Json(()))
}
