use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Json, State};
use axum::response::IntoResponse;
use axum::routing::post;
use bitcoin::FeeRate;
use hex::ToHex;
use ldk_node::lightning::ln::msgs::SocketAddress;
use ldk_node::lightning::routing::gossip::NodeId;
use ldk_node::payment::{PaymentKind, PaymentStatus};
use lightning_invoice::{Bolt11InvoiceDescription as LdkBolt11InvoiceDescription, Description};
use picomint_client::Client;
use picomint_client::gw::Preimage;
use picomint_core::config::FederationId;
use picomint_core::task::TaskHandle;
use picomint_gateway_cli_core::{
    CLI_SOCKET_FILENAME, ChannelInfo, FederationBalanceRequest, FederationBalanceResponse,
    FederationConfigRequest, FederationConfigResponse, FederationInviteResponse,
    FederationJoinRequest, FederationListResponse, FederationMintCountRequest,
    FederationMintCountResponse, FederationMintReceiveRequest, FederationMintReceiveResponse,
    FederationMintSendRequest, FederationMintSendResponse, FederationWalletReceiveRequest,
    FederationWalletReceiveResponse, FederationWalletSendFeeRequest,
    FederationWalletSendFeeResponse, FederationWalletSendRequest, FederationWalletSendResponse,
    InfoResponse, LdkBalancesResponse, LdkChannelCloseRequest, LdkChannelCloseResponse,
    LdkChannelListResponse, LdkChannelOpenRequest, LdkInvoiceCreateRequest,
    LdkInvoiceCreateResponse, LdkInvoicePayRequest, LdkInvoicePayResponse, LdkOnchainReceiveResponse,
    LdkOnchainSendRequest, LdkOnchainSendResponse, LdkPeerConnectRequest, LdkPeerDisconnectRequest,
    LdkPeerListResponse, MnemonicResponse, PeerInfo, QueryRequest, ROUTE_FEDERATION_BALANCE,
    ROUTE_FEDERATION_CONFIG, ROUTE_FEDERATION_INVITE, ROUTE_FEDERATION_JOIN, ROUTE_FEDERATION_LIST,
    ROUTE_FEDERATION_MODULE_MINT_COUNT, ROUTE_FEDERATION_MODULE_MINT_RECEIVE,
    ROUTE_FEDERATION_MODULE_MINT_SEND, ROUTE_FEDERATION_MODULE_WALLET_RECEIVE,
    ROUTE_FEDERATION_MODULE_WALLET_SEND, ROUTE_FEDERATION_MODULE_WALLET_SEND_FEE, ROUTE_INFO,
    ROUTE_LDK_BALANCES, ROUTE_LDK_CHANNEL_CLOSE, ROUTE_LDK_CHANNEL_LIST, ROUTE_LDK_CHANNEL_OPEN,
    ROUTE_LDK_INVOICE_CREATE, ROUTE_LDK_INVOICE_PAY, ROUTE_LDK_ONCHAIN_RECEIVE,
    ROUTE_LDK_ONCHAIN_SEND, ROUTE_LDK_PEER_CONNECT, ROUTE_LDK_PEER_DISCONNECT, ROUTE_LDK_PEER_LIST,
    ROUTE_MNEMONIC, ROUTE_QUERY,
};
use picomint_logging::LOG_GATEWAY;
use reqwest::StatusCode;
use tokio::net::UnixListener;
use tower_http::cors::CorsLayer;
use tracing::{debug, error, info, instrument};

use crate::AppState;

/// Simple error type for CLI/admin endpoints.
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
    pub fn bad_request(error: impl std::fmt::Display) -> Self {
        Self {
            code: StatusCode::BAD_REQUEST,
            error: error.to_string(),
        }
    }

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

impl From<picomint_client::gw::LightningRpcError> for CliError {
    fn from(e: picomint_client::gw::LightningRpcError) -> Self {
        Self::internal(e)
    }
}

impl From<anyhow::Error> for CliError {
    fn from(e: anyhow::Error) -> Self {
        Self::internal(e)
    }
}

pub async fn run_cli(state: AppState, handle: TaskHandle) {
    let socket_path = state.data_dir.join(CLI_SOCKET_FILENAME);
    std::fs::remove_file(&socket_path).ok();

    let listener = UnixListener::bind(&socket_path).expect("Failed to bind CLI server");

    let router = router()
        .with_state(state)
        .layer(CorsLayer::permissive())
        .into_make_service();

    axum::serve(listener, router)
        .with_graceful_shutdown(handle.make_shutdown_rx())
        .await
        .expect("CLI webserver failed");
}

fn router() -> Router<AppState> {
    Router::new()
        // Top-level
        .route(ROUTE_INFO, post(info))
        .route(ROUTE_MNEMONIC, post(mnemonic))
        // LDK node management
        .route(ROUTE_LDK_BALANCES, post(ldk_balances))
        .route(ROUTE_LDK_CHANNEL_OPEN, post(ldk_channel_open))
        .route(ROUTE_LDK_CHANNEL_CLOSE, post(ldk_channel_close))
        .route(ROUTE_LDK_CHANNEL_LIST, post(ldk_channel_list))
        .route(ROUTE_LDK_ONCHAIN_RECEIVE, post(ldk_onchain_receive))
        .route(ROUTE_LDK_ONCHAIN_SEND, post(ldk_onchain_send))
        .route(ROUTE_LDK_INVOICE_CREATE, post(ldk_invoice_create))
        .route(ROUTE_LDK_INVOICE_PAY, post(ldk_invoice_pay))
        .route(ROUTE_LDK_PEER_CONNECT, post(ldk_peer_connect))
        .route(ROUTE_LDK_PEER_DISCONNECT, post(ldk_peer_disconnect))
        .route(ROUTE_LDK_PEER_LIST, post(ldk_peer_list))
        // Federation management
        .route(ROUTE_FEDERATION_JOIN, post(federation_join))
        .route(ROUTE_FEDERATION_LIST, post(federation_list))
        .route(ROUTE_FEDERATION_CONFIG, post(federation_config))
        .route(ROUTE_FEDERATION_INVITE, post(federation_invite))
        .route(ROUTE_FEDERATION_BALANCE, post(federation_balance))
        // Per-federation module commands
        .route(
            ROUTE_FEDERATION_MODULE_MINT_COUNT,
            post(federation_module_mint_count),
        )
        .route(
            ROUTE_FEDERATION_MODULE_MINT_SEND,
            post(federation_module_mint_send),
        )
        .route(
            ROUTE_FEDERATION_MODULE_MINT_RECEIVE,
            post(federation_module_mint_receive),
        )
        .route(
            ROUTE_FEDERATION_MODULE_WALLET_SEND_FEE,
            post(federation_module_wallet_send_fee),
        )
        .route(
            ROUTE_FEDERATION_MODULE_WALLET_SEND,
            post(federation_module_wallet_send),
        )
        .route(
            ROUTE_FEDERATION_MODULE_WALLET_RECEIVE,
            post(federation_module_wallet_receive),
        )
        // Analytics
        .route(ROUTE_QUERY, post(query))
}

/// Run a SQL query against the in-memory gw-events analytics tables.
/// Returns a JSON array of row objects keyed by column name.
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn query(
    State(state): State<AppState>,
    Json(payload): Json<QueryRequest>,
) -> Result<Json<serde_json::Value>, CliError> {
    let rows = crate::query::run_query(&state.query_state, &payload.sql)
        .await
        .map_err(CliError::bad_request)?;
    Ok(Json(rows))
}

// ---------------------------------------------------------------------------
// Top-level handlers
// ---------------------------------------------------------------------------

/// Display high-level information about the Gateway
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn info(State(state): State<AppState>) -> Result<Json<InfoResponse>, CliError> {
    let node_status = state.node.status();

    Ok(Json(InfoResponse {
        public_key: state.node.node_id(),
        alias: state
            .node
            .node_alias()
            .expect("node alias is set")
            .to_string(),
        network: state.node.config().network.to_string(),
        block_height: u64::from(node_status.current_best_block.height),
        synced_to_chain: node_status.latest_lightning_wallet_sync_timestamp.is_some(),
    }))
}

/// Returns the gateway's mnemonic words
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn mnemonic(State(state): State<AppState>) -> Result<Json<MnemonicResponse>, CliError> {
    let words = state
        .client_factory
        .mnemonic()
        .words()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>();

    Ok(Json(MnemonicResponse { mnemonic: words }))
}

// ---------------------------------------------------------------------------
// LDK node management handlers
// ---------------------------------------------------------------------------

/// Returns the onchain and lightning channel capacity balances
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn ldk_balances(
    State(state): State<AppState>,
) -> Result<Json<LdkBalancesResponse>, CliError> {
    let node_balances = state.node.list_balances();

    let channels = state.node.list_channels();
    let total_inbound_capacity_msat: u64 = channels
        .iter()
        .filter(|chan| chan.is_usable)
        .map(|channel| channel.inbound_capacity_msat)
        .sum();
    let total_outbound_capacity_msat: u64 = channels
        .iter()
        .filter(|chan| chan.is_usable)
        .map(|channel| channel.outbound_capacity_msat)
        .sum();

    Ok(Json(LdkBalancesResponse {
        total_onchain_balance_sats: node_balances.total_onchain_balance_sats,
        total_inbound_capacity_msat,
        total_outbound_capacity_msat,
    }))
}

/// Opens a Lightning channel to a peer
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn ldk_channel_open(
    State(state): State<AppState>,
    Json(payload): Json<LdkChannelOpenRequest>,
) -> Result<Json<()>, CliError> {
    let push_amount_msats = if payload.push_amount_sats == 0 {
        None
    } else {
        Some(payload.push_amount_sats * 1000)
    };

    state
        .node
        .open_announced_channel(
            payload.pubkey,
            SocketAddress::from_str(&payload.host)
                .map_err(|e| CliError::internal(format!("Invalid address: {e}")))?,
            payload.channel_size_sats,
            push_amount_msats,
            None,
        )
        .map_err(|e| CliError::internal(format!("Failed to open channel: {e}")))?;

    info!(target: LOG_GATEWAY, pubkey = %payload.pubkey, "Initiated channel open");
    Ok(Json(()))
}

/// Closes all channels with a peer
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn ldk_channel_close(
    State(state): State<AppState>,
    Json(payload): Json<LdkChannelCloseRequest>,
) -> Result<Json<LdkChannelCloseResponse>, CliError> {
    let mut num_channels_closed = 0;
    for channel_with_peer in state
        .node
        .list_channels()
        .iter()
        .filter(|channel| channel.counterparty_node_id == payload.pubkey)
    {
        if payload.force {
            match state.node.force_close_channel(
                &channel_with_peer.user_channel_id,
                payload.pubkey,
                Some("User initiated force close".to_string()),
            ) {
                Ok(()) => num_channels_closed += 1,
                Err(err) => {
                    error!(
                        pubkey = %payload.pubkey,
                        err = %err,
                        "Could not force close channel",
                    );
                }
            }
        } else {
            match state
                .node
                .close_channel(&channel_with_peer.user_channel_id, payload.pubkey)
            {
                Ok(()) => num_channels_closed += 1,
                Err(err) => {
                    error!(
                        pubkey = %payload.pubkey,
                        err = %err,
                        "Could not close channel",
                    );
                }
            }
        }
    }

    info!(target: LOG_GATEWAY, pubkey = %payload.pubkey, "Initiated channel closure");
    let response = LdkChannelCloseResponse {
        num_channels_closed,
    };
    Ok(Json(response))
}

/// Lists all Lightning channels
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn ldk_channel_list(
    State(state): State<AppState>,
) -> Result<Json<LdkChannelListResponse>, CliError> {
    let mut channels = Vec::new();
    let network_graph = state.node.network_graph();

    let peer_addresses: std::collections::HashMap<_, _> = state
        .node
        .list_peers()
        .into_iter()
        .map(|peer| (peer.node_id, peer.address.to_string()))
        .collect();

    for channel_details in &state.node.list_channels() {
        let node_id = NodeId::from_pubkey(&channel_details.counterparty_node_id);
        let node_info = network_graph.node(&node_id);

        let remote_node_alias = node_info.as_ref().and_then(|info| {
            info.announcement_info.as_ref().and_then(|announcement| {
                let alias = announcement.alias().to_string();
                if alias.is_empty() { None } else { Some(alias) }
            })
        });

        let remote_address = peer_addresses
            .get(&channel_details.counterparty_node_id)
            .cloned();

        channels.push(ChannelInfo {
            remote_pubkey: channel_details.counterparty_node_id,
            remote_alias: remote_node_alias,
            remote_address,
            channel_size_sats: channel_details.channel_value_sats,
            outbound_liquidity_sats: channel_details.outbound_capacity_msat / 1000,
            inbound_liquidity_sats: channel_details.inbound_capacity_msat / 1000,
            is_usable: channel_details.is_usable,
            is_outbound: channel_details.is_outbound,
            funding_txid: channel_details.funding_txo.map(|txo| txo.txid),
        });
    }

    Ok(Json(LdkChannelListResponse { channels }))
}

/// Generates an onchain address to fund the gateway's lightning node
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn ldk_onchain_receive(
    State(state): State<AppState>,
) -> Result<Json<LdkOnchainReceiveResponse>, CliError> {
    let address = state
        .node
        .onchain_payment()
        .new_address()
        .map_err(|e| CliError::internal(format!("Failed to get onchain address: {e}")))?;

    Ok(Json(LdkOnchainReceiveResponse {
        address: address.as_unchecked().clone(),
    }))
}

/// Send funds from the gateway's lightning node on-chain wallet
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn ldk_onchain_send(
    State(state): State<AppState>,
    Json(payload): Json<LdkOnchainSendRequest>,
) -> Result<Json<LdkOnchainSendResponse>, CliError> {
    let onchain = state.node.onchain_payment();
    let checked_address = payload.address.clone().assume_checked();
    let txid = onchain
        .send_to_address(
            &checked_address,
            payload.amount.to_sat(),
            FeeRate::from_sat_per_vb(payload.sats_per_vbyte),
        )
        .map_err(|e| CliError::internal(format!("Withdraw error: {e}")))?;
    info!(target: LOG_GATEWAY, txid = %txid, "Sent onchain transaction");
    Ok(Json(LdkOnchainSendResponse { txid }))
}

/// Creates an invoice directly payable to the gateway's lightning node
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn ldk_invoice_create(
    State(state): State<AppState>,
    Json(payload): Json<LdkInvoiceCreateRequest>,
) -> Result<Json<LdkInvoiceCreateResponse>, CliError> {
    let expiry_secs = payload.expiry_secs.unwrap_or(3600);
    let description = match payload.description {
        Some(desc) => LdkBolt11InvoiceDescription::Direct(
            Description::new(desc)
                .map_err(|_| CliError::internal("Invalid invoice description"))?,
        ),
        None => LdkBolt11InvoiceDescription::Direct(Description::empty()),
    };

    let invoice = state
        .node
        .bolt11_payment()
        .receive(payload.amount_msats, &description, expiry_secs)
        .map_err(|e| CliError::internal(format!("Failed to get invoice: {e}")))?;

    Ok(Json(LdkInvoiceCreateResponse {
        invoice: invoice.to_string(),
    }))
}

/// Pays an outgoing LN invoice using the gateway's own funds
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn ldk_invoice_pay(
    State(state): State<AppState>,
    Json(payload): Json<LdkInvoicePayRequest>,
) -> Result<Json<LdkInvoicePayResponse>, CliError> {
    let payment_id = state
        .node
        .bolt11_payment()
        .send(&payload.invoice, None)
        .map_err(|e| CliError::internal(format!("LDK payment failed to initialize: {e:?}")))?;

    let preimage = loop {
        if let Some(payment_details) = state.node.payment(&payment_id) {
            match payment_details.status {
                PaymentStatus::Pending => {}
                PaymentStatus::Succeeded => {
                    if let PaymentKind::Bolt11 {
                        preimage: Some(preimage),
                        ..
                    } = payment_details.kind
                    {
                        break Preimage(preimage.0);
                    }
                }
                PaymentStatus::Failed => {
                    return Err(CliError::internal("LDK payment failed"));
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    Ok(Json(LdkInvoicePayResponse {
        preimage: preimage.0.encode_hex::<String>(),
    }))
}

/// Connects to a Lightning peer
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn ldk_peer_connect(
    State(state): State<AppState>,
    Json(payload): Json<LdkPeerConnectRequest>,
) -> Result<Json<()>, CliError> {
    let address: SocketAddress = payload
        .host
        .parse()
        .map_err(|e| CliError::bad_request(format!("Invalid address: {e}")))?;

    state
        .node
        .connect(payload.pubkey, address, true)
        .map_err(|e| CliError::internal(format!("Failed to connect to peer: {e}")))?;

    info!(target: LOG_GATEWAY, pubkey = %payload.pubkey, "Connected to peer");
    Ok(Json(()))
}

/// Disconnects from a Lightning peer
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn ldk_peer_disconnect(
    State(state): State<AppState>,
    Json(payload): Json<LdkPeerDisconnectRequest>,
) -> Result<Json<()>, CliError> {
    state
        .node
        .disconnect(payload.pubkey)
        .map_err(|e| CliError::internal(format!("Failed to disconnect from peer: {e}")))?;

    info!(target: LOG_GATEWAY, pubkey = %payload.pubkey, "Disconnected from peer");
    Ok(Json(()))
}

/// Lists all Lightning peers
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn ldk_peer_list(
    State(state): State<AppState>,
) -> Result<Json<LdkPeerListResponse>, CliError> {
    let peers = state
        .node
        .list_peers()
        .into_iter()
        .map(|peer| PeerInfo {
            node_id: peer.node_id,
            address: peer.address.to_string(),
            is_connected: peer.is_connected,
        })
        .collect();

    Ok(Json(LdkPeerListResponse { peers }))
}

// ---------------------------------------------------------------------------
// Federation management handlers
// ---------------------------------------------------------------------------

/// Join a new federation
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn federation_join(
    State(state): State<AppState>,
    Json(payload): Json<FederationJoinRequest>,
) -> Result<Json<()>, CliError> {
    let invite_code: picomint_core::invite_code::InviteCode =
        picomint_base32::decode(&payload.invite).map_err(|e| {
            CliError::bad_request(format!("Invalid federation member string {e:?}"))
        })?;

    if state.clients.read().await.contains_key(&invite_code.federation_id) {
        return Err(CliError::bad_request(
            "Federation has already been registered",
        ));
    }

    let client = state
        .client_factory
        .join(&invite_code, Arc::new(state.clone()))
        .await?;

    AppState::check_federation_network(&client, state.network).await?;

    state
        .clients
        .write()
        .await
        .insert(invite_code.federation_id, client.clone());

    crate::query::spawn_tail(
        &state.task_group,
        client,
        invite_code.federation_id,
        state.query_state.clone(),
    );

    debug!(target: LOG_GATEWAY, federation_id = %invite_code.federation_id, "Federation connected");

    Ok(Json(()))
}

/// List connected federations
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn federation_list(
    State(state): State<AppState>,
) -> Result<Json<FederationListResponse>, CliError> {
    let federations = state.federation_info_all().await;
    Ok(Json(FederationListResponse { federations }))
}

/// Display federation config
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn federation_config(
    State(state): State<AppState>,
    Json(_payload): Json<FederationConfigRequest>,
) -> Result<Json<FederationConfigResponse>, CliError> {
    let federations = state.all_federation_configs().await;
    Ok(Json(FederationConfigResponse { federations }))
}

/// Get a federation's ecash balance
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn federation_balance(
    State(state): State<AppState>,
    Json(payload): Json<FederationBalanceRequest>,
) -> Result<Json<FederationBalanceResponse>, CliError> {
    let client = resolve_client(&state, payload.federation_id).await?;

    let balance_msat = client
        .get_balance()
        .await
        .map_err(|e| CliError::internal(format!("Failed to read balance: {e}")))?;

    Ok(Json(FederationBalanceResponse { balance_msat }))
}

/// Export invite codes for all connected federations
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn federation_invite(
    State(state): State<AppState>,
) -> Result<Json<FederationInviteResponse>, CliError> {
    let invite_codes = state.all_invite_codes().await;
    Ok(Json(FederationInviteResponse { invite_codes }))
}

// ---------------------------------------------------------------------------
// Per-federation module handlers
// ---------------------------------------------------------------------------

/// Resolve the target federation client. When `id` is `None` and the gateway
/// has exactly one federation joined, that one is used; otherwise the caller
/// must supply `--id`.
async fn resolve_client(
    state: &AppState,
    id: Option<FederationId>,
) -> Result<Arc<Client>, CliError> {
    let clients = state.clients.read().await;
    match id {
        Some(id) => clients
            .get(&id)
            .cloned()
            .ok_or_else(|| CliError::bad_request("Federation not connected")),
        None => {
            if clients.len() > 1 {
                return Err(CliError::bad_request(
                    "Multiple federations connected — pass --id <FEDERATION_ID>",
                ));
            }
            clients
                .values()
                .next()
                .cloned()
                .ok_or_else(|| CliError::bad_request("No federations connected"))
        }
    }
}

/// Count held ecash notes by denomination
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn federation_module_mint_count(
    State(state): State<AppState>,
    Json(payload): Json<FederationMintCountRequest>,
) -> Result<Json<FederationMintCountResponse>, CliError> {
    let client = resolve_client(&state, payload.federation_id).await?;
    let counts = client.mint().get_count_by_denomination();
    Ok(Json(FederationMintCountResponse { counts }))
}

/// Spend ecash from a federation
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn federation_module_mint_send(
    State(state): State<AppState>,
    Json(payload): Json<FederationMintSendRequest>,
) -> Result<Json<FederationMintSendResponse>, CliError> {
    let client = resolve_client(&state, payload.federation_id).await?;

    let ecash = client
        .mint()
        .send(picomint_core::Amount::from_sats(payload.amount.to_sat()))
        .await
        .map_err(CliError::internal)?;

    Ok(Json(FederationMintSendResponse {
        notes: picomint_base32::encode(&ecash),
    }))
}

/// Receive ecash into the gateway. The ecash string itself carries the target
/// federation id, so no `--id` is needed.
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn federation_module_mint_receive(
    State(state): State<AppState>,
    Json(payload): Json<FederationMintReceiveRequest>,
) -> Result<Json<FederationMintReceiveResponse>, CliError> {
    let ecash: picomint_client::mint::ECash = picomint_base32::decode(&payload.notes)
        .map_err(|e| CliError::bad_request(format!("Invalid ECash: {e}")))?;

    let client = state
        .select_client(ecash.mint)
        .await
        .ok_or(CliError::bad_request("Federation not connected"))?;

    let amount = ecash.amount();

    client
        .mint()
        .receive(&ecash)
        .map_err(|e| CliError::internal(format!("Failed to receive ecash: {e}")))?;

    Ok(Json(FederationMintReceiveResponse { amount }))
}

/// Fetch the current onchain send-fee for a federation
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn federation_module_wallet_send_fee(
    State(state): State<AppState>,
    Json(payload): Json<FederationWalletSendFeeRequest>,
) -> Result<Json<FederationWalletSendFeeResponse>, CliError> {
    let client = resolve_client(&state, payload.federation_id).await?;
    let fee = client
        .wallet()
        .send_fee()
        .await
        .map_err(|e| CliError::internal(format!("Failed to fetch send fee: {e}")))?;
    Ok(Json(FederationWalletSendFeeResponse { fee }))
}

/// Withdraw onchain from a federation
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn federation_module_wallet_send(
    State(state): State<AppState>,
    Json(payload): Json<FederationWalletSendRequest>,
) -> Result<Json<FederationWalletSendResponse>, CliError> {
    let client = resolve_client(&state, payload.federation_id).await?;
    let operation_id = client
        .wallet()
        .send(payload.address, payload.amount, payload.fee)
        .await
        .map_err(|e| CliError::internal(format!("Failed to send onchain: {e}")))?;
    Ok(Json(FederationWalletSendResponse { operation_id }))
}

/// Generate deposit address for a federation
#[instrument(target = LOG_GATEWAY, skip_all, err)]
async fn federation_module_wallet_receive(
    State(state): State<AppState>,
    Json(payload): Json<FederationWalletReceiveRequest>,
) -> Result<Json<FederationWalletReceiveResponse>, CliError> {
    let client = resolve_client(&state, payload.federation_id).await?;
    let address = client.wallet().receive().await;
    Ok(Json(FederationWalletReceiveResponse {
        address: address.as_unchecked().clone(),
    }))
}
