use std::str::FromStr;
use std::time::Duration;

use axum::Router;
use axum::extract::{Json, State};
use axum::response::IntoResponse;
use axum::routing::post;
use bitcoin::FeeRate;
use futures::StreamExt as _;
use hex::ToHex;
use ldk_node::lightning::ln::msgs::SocketAddress;
use ldk_node::lightning::routing::gossip::NodeId;
use ldk_node::payment::{PaymentKind, PaymentStatus};
use ldk_node::{PendingSweepBalance, UserChannelId};
use lightning_invoice::{Bolt11InvoiceDescription as LdkBolt11InvoiceDescription, Description};
use picomint_client::gw::GATEWAY_ACCOUNT;
use picomint_client::wallet::events::{SendFailureEvent, SendSuccessEvent};
use picomint_client::{TxAcceptEvent, TxRejectEvent};
use picomint_core::config::FederationId;
use picomint_core::ln::gateway::GatewayPk;
use picomint_gateway_cli_core::{
    AnalyticsRequest, AnalyticsResponse, CLI_SOCKET_FILENAME, ChannelInfo, FederationAddRequest,
    FederationBalanceRequest, FederationBalanceResponse, FederationConfigRequest,
    FederationConfigResponse, FederationDisableRequest, FederationEnableRequest,
    FederationListResponse, FederationMintCountRequest, FederationMintCountResponse,
    FederationMintReceiveRequest, FederationMintReceiveResponse, FederationMintSendRequest,
    FederationMintSendResponse, FederationWalletReceiveRequest, FederationWalletReceiveResponse,
    FederationWalletSendFeeRequest, FederationWalletSendFeeResponse, FederationWalletSendRequest,
    FederationWalletSendResponse, InfoResponse, LdkBalancesResponse, LdkChannelCloseRequest,
    LdkChannelListResponse, LdkChannelOpenRequest, LdkChannelSpliceInRequest,
    LdkChannelSpliceOutRequest, LdkLnProbeRequest, LdkLnReceiveRequest, LdkLnReceiveResponse,
    LdkLnSendRequest, LdkLnSendResponse, LdkOnchainReceiveResponse, LdkOnchainSendRequest,
    LdkOnchainSendResponse, LdkPeerConnectRequest, LdkPeerDisconnectRequest, LdkPeerListResponse,
    MnemonicResponse, PeerInfo, ROUTE_ANALYTICS, ROUTE_FEDERATION_ADD, ROUTE_FEDERATION_BALANCE,
    ROUTE_FEDERATION_CONFIG, ROUTE_FEDERATION_DISABLE, ROUTE_FEDERATION_ENABLE,
    ROUTE_FEDERATION_LIST, ROUTE_FEDERATION_MODULE_MINT_COUNT,
    ROUTE_FEDERATION_MODULE_MINT_RECEIVE, ROUTE_FEDERATION_MODULE_MINT_SEND,
    ROUTE_FEDERATION_MODULE_WALLET_RECEIVE, ROUTE_FEDERATION_MODULE_WALLET_SEND,
    ROUTE_FEDERATION_MODULE_WALLET_SEND_FEE, ROUTE_INFO, ROUTE_LDK_BALANCES,
    ROUTE_LDK_CHANNEL_CLOSE, ROUTE_LDK_CHANNEL_LIST, ROUTE_LDK_CHANNEL_OPEN,
    ROUTE_LDK_CHANNEL_SPLICE_IN, ROUTE_LDK_CHANNEL_SPLICE_OUT, ROUTE_LDK_LN_PROBE,
    ROUTE_LDK_LN_RECEIVE, ROUTE_LDK_LN_SEND, ROUTE_LDK_ONCHAIN_RECEIVE, ROUTE_LDK_ONCHAIN_SEND,
    ROUTE_LDK_PEER_CONNECT, ROUTE_LDK_PEER_DISCONNECT, ROUTE_LDK_PEER_LIST, ROUTE_MNEMONIC,
};
use reqwest::StatusCode;
use tokio::net::UnixListener;
use tower_http::cors::CorsLayer;
use tracing::{info, instrument};

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

impl From<anyhow::Error> for CliError {
    fn from(e: anyhow::Error) -> Self {
        Self::internal(e)
    }
}

pub async fn run_cli(state: AppState) {
    let socket_path = state.data_dir.join(CLI_SOCKET_FILENAME);
    std::fs::remove_file(&socket_path).ok();

    let listener = UnixListener::bind(&socket_path).expect("Failed to bind CLI server");

    let router = router()
        .with_state(state)
        .layer(CorsLayer::permissive())
        .into_make_service();

    axum::serve(listener, router)
        .await
        .expect("CLI webserver failed");
}

fn router() -> Router<AppState> {
    Router::new()
        // Top-level
        .route(ROUTE_INFO, post(info))
        .route(ROUTE_MNEMONIC, post(mnemonic))
        .route(ROUTE_ANALYTICS, post(analytics))
        // LDK node management
        .route(ROUTE_LDK_BALANCES, post(ldk_balances))
        .route(ROUTE_LDK_CHANNEL_OPEN, post(ldk_channel_open))
        .route(ROUTE_LDK_CHANNEL_CLOSE, post(ldk_channel_close))
        .route(ROUTE_LDK_CHANNEL_LIST, post(ldk_channel_list))
        .route(ROUTE_LDK_CHANNEL_SPLICE_IN, post(ldk_channel_splice_in))
        .route(ROUTE_LDK_CHANNEL_SPLICE_OUT, post(ldk_channel_splice_out))
        .route(ROUTE_LDK_ONCHAIN_RECEIVE, post(ldk_onchain_receive))
        .route(ROUTE_LDK_ONCHAIN_SEND, post(ldk_onchain_send))
        .route(ROUTE_LDK_LN_RECEIVE, post(ldk_ln_receive))
        .route(ROUTE_LDK_LN_SEND, post(ldk_ln_send))
        .route(ROUTE_LDK_LN_PROBE, post(ldk_ln_probe))
        .route(ROUTE_LDK_PEER_CONNECT, post(ldk_peer_connect))
        .route(ROUTE_LDK_PEER_DISCONNECT, post(ldk_peer_disconnect))
        .route(ROUTE_LDK_PEER_LIST, post(ldk_peer_list))
        // Federation management
        .route(ROUTE_FEDERATION_ADD, post(federation_add))
        .route(ROUTE_FEDERATION_DISABLE, post(federation_disable))
        .route(ROUTE_FEDERATION_ENABLE, post(federation_enable))
        .route(ROUTE_FEDERATION_LIST, post(federation_list))
        .route(ROUTE_FEDERATION_CONFIG, post(federation_config))
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
}

// ---------------------------------------------------------------------------
// Top-level handlers
// ---------------------------------------------------------------------------

/// Display high-level information about the Gateway
#[instrument(skip_all, err)]
async fn info(State(state): State<AppState>) -> Result<Json<InfoResponse>, CliError> {
    let node_status = state.node.status();

    Ok(Json(InfoResponse {
        lightning_pk: state.node.node_id(),
        gateway_pk: GatewayPk(state.endpoint.id()),
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
#[instrument(skip_all, err)]
async fn mnemonic(State(state): State<AppState>) -> Result<Json<MnemonicResponse>, CliError> {
    let words = state
        .mnemonic
        .words()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>();

    Ok(Json(MnemonicResponse { mnemonic: words }))
}

async fn analytics(
    State(state): State<AppState>,
    Json(request): Json<AnalyticsRequest>,
) -> Result<Json<AnalyticsResponse>, CliError> {
    let rows = tokio::task::spawn_blocking(move || {
        crate::analytics::query(&state.data_dir, &request.query)
    })
    .await
    .map_err(CliError::internal)?
    .map_err(CliError::bad_request)?;

    Ok(Json(rows))
}

// ---------------------------------------------------------------------------
// LDK node management handlers
// ---------------------------------------------------------------------------

/// Returns the onchain and lightning channel capacity balances
#[instrument(skip_all, err)]
async fn ldk_balances(
    State(state): State<AppState>,
) -> Result<Json<LdkBalancesResponse>, CliError> {
    let node_balances = state.node.list_balances();

    // A channel that is not usable — still awaiting its funding confirmation,
    // or its peer disconnected — carries no payment in either direction, so it
    // contributes to none of the three capacities below.
    let usable_channels = state
        .node
        .list_channels()
        .into_iter()
        .filter(|channel| channel.is_usable)
        .collect::<Vec<_>>();

    let total_inbound_capacity_sat: u64 = usable_channels
        .iter()
        .map(|channel| channel.inbound_capacity_msat / 1000)
        .sum();

    let total_outbound_capacity_sat: u64 = usable_channels
        .iter()
        .map(|channel| channel.outbound_capacity_msat / 1000)
        .sum();

    let total_next_outbound_htlc_limit_sat: u64 = usable_channels
        .iter()
        .map(|channel| channel.next_outbound_htlc_limit_msat / 1000)
        .sum();

    let total_pending_closure_balance_sat = node_balances
        .pending_balances_from_channel_closures
        .iter()
        .map(|balance| match balance {
            PendingSweepBalance::PendingBroadcast {
                amount_satoshis, ..
            }
            | PendingSweepBalance::BroadcastAwaitingConfirmation {
                amount_satoshis, ..
            }
            | PendingSweepBalance::AwaitingThresholdConfirmations {
                amount_satoshis, ..
            } => *amount_satoshis,
        })
        .sum();

    Ok(Json(LdkBalancesResponse {
        total_onchain_balance_sat: node_balances.total_onchain_balance_sats,
        spendable_onchain_balance_sat: node_balances.spendable_onchain_balance_sats,
        total_anchor_channels_reserve_sat: node_balances.total_anchor_channels_reserve_sats,
        total_inbound_capacity_sat,
        total_outbound_capacity_sat,
        total_next_outbound_htlc_limit_sat,
        total_lightning_balance_sat: node_balances.total_lightning_balance_sats,
        total_pending_closure_balance_sat,
    }))
}

/// Opens a Lightning channel to a peer
#[instrument(skip_all, err)]
async fn ldk_channel_open(
    State(state): State<AppState>,
    Json(payload): Json<LdkChannelOpenRequest>,
) -> Result<Json<()>, CliError> {
    let push_amount_msat = if payload.push_amount_sat == 0 {
        None
    } else {
        Some(payload.push_amount_sat * 1000)
    };

    // Unannounced by default, matching LDK; a gateway only needs its peers to
    // route to it, not the wider network.
    let open_channel = if payload.announce {
        ldk_node::Node::open_announced_channel
    } else {
        ldk_node::Node::open_channel
    };

    open_channel(
        &state.node,
        payload.pubkey,
        SocketAddress::from_str(&payload.host)
            .map_err(|e| CliError::internal(format!("Invalid address: {e}")))?,
        payload.channel_size_sat,
        push_amount_msat,
        None,
    )
    .map_err(|e| CliError::internal(format!("Failed to open channel: {e}")))?;

    info!(pubkey = %payload.pubkey, announce = payload.announce, "Initiated channel open");
    Ok(Json(()))
}

/// Closes a channel. The channel is named by its `user_channel_id` rather
/// than by peer, since a peer may hold several; `channel list` reports both
/// fields.
#[instrument(skip_all, err)]
async fn ldk_channel_close(
    State(state): State<AppState>,
    Json(payload): Json<LdkChannelCloseRequest>,
) -> Result<Json<()>, CliError> {
    let user_channel_id = UserChannelId(payload.user_channel_id);

    if payload.force {
        state
            .node
            .force_close_channel(
                &user_channel_id,
                payload.pubkey,
                Some("User initiated force close".to_string()),
            )
            .map_err(|e| CliError::internal(format!("Failed to force close channel: {e}")))?;
    } else {
        state
            .node
            .close_channel(&user_channel_id, payload.pubkey)
            .map_err(|e| CliError::internal(format!("Failed to close channel: {e}")))?;
    }

    info!(
        user_channel_id = payload.user_channel_id,
        pubkey = %payload.pubkey,
        force = payload.force,
        "Initiated channel closure"
    );

    Ok(Json(()))
}

/// Splices on-chain funds into the channel with a peer, growing its capacity
/// without closing it. Experimental; the counterparty must support splicing.
#[instrument(skip_all, err)]
async fn ldk_channel_splice_in(
    State(state): State<AppState>,
    Json(payload): Json<LdkChannelSpliceInRequest>,
) -> Result<Json<()>, CliError> {
    state
        .node
        .splice_in(
            &UserChannelId(payload.user_channel_id),
            payload.pubkey,
            payload.amount_sat,
        )
        .map_err(|e| CliError::internal(format!("Failed to splice in: {e}")))?;

    info!(
        user_channel_id = payload.user_channel_id,
        pubkey = %payload.pubkey,
        amount_sat = payload.amount_sat,
        "Initiated splice-in"
    );

    Ok(Json(()))
}

/// Splices funds out of a channel to an on-chain address without closing it.
/// Experimental; the amount must not exceed the channel's outbound capacity
/// and the counterparty must support splicing.
#[instrument(skip_all, err)]
async fn ldk_channel_splice_out(
    State(state): State<AppState>,
    Json(payload): Json<LdkChannelSpliceOutRequest>,
) -> Result<Json<()>, CliError> {
    state
        .node
        .splice_out(
            &UserChannelId(payload.user_channel_id),
            payload.pubkey,
            &payload.address.assume_checked(),
            payload.amount_sat,
        )
        .map_err(|e| CliError::internal(format!("Failed to splice out: {e}")))?;

    info!(
        user_channel_id = payload.user_channel_id,
        pubkey = %payload.pubkey,
        amount_sat = payload.amount_sat,
        "Initiated splice-out"
    );

    Ok(Json(()))
}

/// Lists all Lightning channels
#[instrument(skip_all, err)]
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
            user_channel_id: channel_details.user_channel_id.0,
            remote_pubkey: channel_details.counterparty_node_id,
            remote_alias: remote_node_alias,
            remote_address,
            channel_size_sat: channel_details.channel_value_sats,
            outbound_liquidity_sat: channel_details.outbound_capacity_msat / 1000,
            next_outbound_htlc_limit_sat: channel_details.next_outbound_htlc_limit_msat / 1000,
            inbound_liquidity_sat: channel_details.inbound_capacity_msat / 1000,
            is_usable: channel_details.is_usable,
            is_outbound: channel_details.is_outbound,
            is_announced: channel_details.is_announced,
            funding_txid: channel_details.funding_txo.map(|txo| txo.txid),
        });
    }

    Ok(Json(LdkChannelListResponse { channels }))
}

/// Generates an onchain address to fund the gateway's lightning node
#[instrument(skip_all, err)]
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
#[instrument(skip_all, err)]
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
            FeeRate::from_sat_per_vb(payload.sat_per_vbyte),
        )
        .map_err(|e| CliError::internal(format!("Withdraw error: {e}")))?;
    info!(txid = %txid, "Sent onchain transaction");
    Ok(Json(LdkOnchainSendResponse { txid }))
}

/// Creates an invoice directly payable to the gateway's lightning node
#[instrument(skip_all, err)]
async fn ldk_ln_receive(
    State(state): State<AppState>,
    Json(payload): Json<LdkLnReceiveRequest>,
) -> Result<Json<LdkLnReceiveResponse>, CliError> {
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
        .receive(payload.amount_msat, &description, expiry_secs)
        .map_err(|e| CliError::internal(format!("Failed to get invoice: {e}")))?;

    Ok(Json(LdkLnReceiveResponse {
        invoice: invoice.to_string(),
    }))
}

/// Pays an outgoing LN invoice using the gateway's own funds
#[instrument(skip_all, err)]
async fn ldk_ln_send(
    State(state): State<AppState>,
    Json(payload): Json<LdkLnSendRequest>,
) -> Result<Json<LdkLnSendResponse>, CliError> {
    let payment_id = state
        .node
        .bolt11_payment()
        .send(&payload.invoice, None)
        .map_err(|e| CliError::internal(format!("LDK payment failed to initialize: {e:?}")))?;

    let preimage: [u8; 32] = loop {
        if let Some(payment_details) = state.node.payment(&payment_id) {
            match payment_details.status {
                PaymentStatus::Pending => {}
                PaymentStatus::Succeeded => {
                    if let PaymentKind::Bolt11 {
                        preimage: Some(preimage),
                        ..
                    } = payment_details.kind
                    {
                        break preimage.0;
                    }
                }
                PaymentStatus::Failed => {
                    return Err(CliError::internal("LDK payment failed"));
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    Ok(Json(LdkLnSendResponse {
        preimage: preimage.encode_hex::<String>(),
    }))
}

/// Sends payment probes over all routes towards a node for the given amount,
/// to exercise pathfinding and warm the scorer without moving funds. Probe
/// outcomes surface only in the daemon's LDK logs, so nothing meaningful is
/// returned here.
#[instrument(skip_all, err)]
async fn ldk_ln_probe(
    State(state): State<AppState>,
    Json(payload): Json<LdkLnProbeRequest>,
) -> Result<Json<()>, CliError> {
    state
        .node
        .spontaneous_payment()
        .send_probes(payload.amount_msat, payload.node_id)
        .map_err(|e| CliError::internal(format!("Failed to send probes: {e}")))?;

    Ok(Json(()))
}

/// Connects to a Lightning peer
#[instrument(skip_all, err)]
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

    info!(pubkey = %payload.pubkey, "Connected to peer");
    Ok(Json(()))
}

/// Disconnects from a Lightning peer
#[instrument(skip_all, err)]
async fn ldk_peer_disconnect(
    State(state): State<AppState>,
    Json(payload): Json<LdkPeerDisconnectRequest>,
) -> Result<Json<()>, CliError> {
    state
        .node
        .disconnect(payload.pubkey)
        .map_err(|e| CliError::internal(format!("Failed to disconnect from peer: {e}")))?;

    info!(pubkey = %payload.pubkey, "Disconnected from peer");
    Ok(Json(()))
}

/// Lists all Lightning peers
#[instrument(skip_all, err)]
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

/// Add a new federation
#[instrument(skip_all, err)]
async fn federation_add(
    State(state): State<AppState>,
    Json(payload): Json<FederationAddRequest>,
) -> Result<Json<()>, CliError> {
    state
        .client
        .add(&payload.invite, Some(state.network))
        .await?;

    Ok(Json(()))
}

/// Disable a federation's public client API. Blind insert into
/// `DisabledFederationTable` — no validation of whether the fed is even
/// added.
#[instrument(skip_all, err)]
async fn federation_disable(
    State(state): State<AppState>,
    Json(payload): Json<FederationDisableRequest>,
) -> Result<Json<()>, CliError> {
    let dbtx = state.gateway_db.begin_write();
    dbtx.insert(
        &crate::db::DisabledFederationTable,
        &payload.federation,
        &(),
    );
    dbtx.commit();

    Ok(Json(()))
}

/// Re-enable a previously disabled federation. Blind remove from
/// `DisabledFederationTable` — no-op if the row isn't there.
#[instrument(skip_all, err)]
async fn federation_enable(
    State(state): State<AppState>,
    Json(payload): Json<FederationEnableRequest>,
) -> Result<Json<()>, CliError> {
    let dbtx = state.gateway_db.begin_write();
    dbtx.remove(&crate::db::DisabledFederationTable, &payload.federation);
    dbtx.commit();

    Ok(Json(()))
}

/// List connected federations
#[instrument(skip_all, err)]
async fn federation_list(
    State(state): State<AppState>,
) -> Result<Json<FederationListResponse>, CliError> {
    Ok(Json(FederationListResponse {
        federations: state.federation_list(),
    }))
}

/// Display federation config
#[instrument(skip_all, err)]
async fn federation_config(
    State(state): State<AppState>,
    Json(payload): Json<FederationConfigRequest>,
) -> Result<Json<FederationConfigResponse>, CliError> {
    let federation = resolve_federation(&state, payload.federation)?;

    let config = state
        .client
        .config(federation)
        .ok_or_else(|| CliError::bad_request("Federation not joined"))?;

    Ok(Json(FederationConfigResponse {
        config: serde_json::to_value(config).expect("ConsensusConfig is serializable"),
    }))
}

/// Get a federation's ecash balance
#[instrument(skip_all, err)]
async fn federation_balance(
    State(state): State<AppState>,
    Json(payload): Json<FederationBalanceRequest>,
) -> Result<Json<FederationBalanceResponse>, CliError> {
    let federation = resolve_federation(&state, payload.federation)?;

    let balance_msat = state.client.mint_balance(federation, GATEWAY_ACCOUNT);

    Ok(Json(FederationBalanceResponse { balance_msat }))
}

// ---------------------------------------------------------------------------
// Per-federation module handlers
// ---------------------------------------------------------------------------

/// Resolve the target federation. When `id` is `None` and the gateway has
/// exactly one federation added, that one is used; otherwise the caller must
/// supply `--id`. Resolves against persisted configs, so an
/// added-but-not-yet-connected federation works on first use.
fn resolve_federation(
    state: &AppState,
    id: Option<FederationId>,
) -> Result<FederationId, CliError> {
    match id {
        Some(id) => Ok(id),
        None => match state.federation_list().as_slice() {
            [] => Err(CliError::bad_request("No federations connected")),
            [info] => Ok(info.federation),
            _ => Err(CliError::bad_request(
                "Multiple federations connected — pass --id <FEDERATION_ID>",
            )),
        },
    }
}

/// Count held ecash notes by denomination
#[instrument(skip_all, err)]
async fn federation_module_mint_count(
    State(state): State<AppState>,
    Json(payload): Json<FederationMintCountRequest>,
) -> Result<Json<FederationMintCountResponse>, CliError> {
    let federation = resolve_federation(&state, payload.federation)?;
    let counts = state
        .client
        .mint_count_by_denomination(federation, GATEWAY_ACCOUNT);
    Ok(Json(FederationMintCountResponse { counts }))
}

/// Spend ecash from a federation
#[instrument(skip_all, err)]
async fn federation_module_mint_send(
    State(state): State<AppState>,
    Json(payload): Json<FederationMintSendRequest>,
) -> Result<Json<FederationMintSendResponse>, CliError> {
    let federation = resolve_federation(&state, payload.federation)?;

    let ecash = state
        .client
        .mint_send(
            federation,
            GATEWAY_ACCOUNT,
            picomint_core::Amount::from_sat(payload.amount.to_sat()),
        )
        .await
        .map_err(CliError::internal)?;

    Ok(Json(FederationMintSendResponse { ecash }))
}

/// Receive ecash into the gateway. The ecash bundle itself carries the target
/// federation id, so no `--id` is needed. Blocks until issuance either
/// completes or fails federation-side.
#[instrument(skip_all, err)]
async fn federation_module_mint_receive(
    State(state): State<AppState>,
    Json(payload): Json<FederationMintReceiveRequest>,
) -> Result<Json<FederationMintReceiveResponse>, CliError> {
    let amount = payload.ecash.amount();

    let operation = state
        .client
        .mint_receive(payload.ecash.mint, GATEWAY_ACCOUNT, &payload.ecash)
        .map_err(|e| CliError::internal(format!("Failed to submit reissue: {e}")))?;

    let mut events = state.client.subscribe_operation_events(operation);
    while let Some(entry) = events.next().await {
        if entry.to_event::<TxAcceptEvent>().is_some() {
            return Ok(Json(FederationMintReceiveResponse { amount }));
        }
        if let Some(e) = entry.to_event::<TxRejectEvent>() {
            return Err(CliError::bad_request(format!(
                "Transaction rejected: {}",
                e.error
            )));
        }
    }
    Err(CliError::internal("Event stream ended unexpectedly"))
}

/// Fetch the current onchain send-fee for a federation
#[instrument(skip_all, err)]
async fn federation_module_wallet_send_fee(
    State(state): State<AppState>,
    Json(payload): Json<FederationWalletSendFeeRequest>,
) -> Result<Json<FederationWalletSendFeeResponse>, CliError> {
    let federation = resolve_federation(&state, payload.federation)?;
    let fee = state
        .client
        .wallet_send_fee(federation)
        .await
        .map_err(|e| CliError::internal(format!("Failed to fetch send fee: {e}")))?;
    Ok(Json(FederationWalletSendFeeResponse { fee }))
}

/// Withdraw onchain from a federation. Blocks until the send reaches a
/// terminal state: confirmed broadcast, federation rejected the input tx, or
/// the federation accepted but never produced a bitcoin txid.
#[instrument(skip_all, err)]
async fn federation_module_wallet_send(
    State(state): State<AppState>,
    Json(payload): Json<FederationWalletSendRequest>,
) -> Result<Json<FederationWalletSendResponse>, CliError> {
    let federation = resolve_federation(&state, payload.federation)?;
    let operation = state
        .client
        .wallet_send(
            federation,
            GATEWAY_ACCOUNT,
            payload.address,
            payload.amount,
            payload.fee,
        )
        .await
        .map_err(|e| CliError::internal(format!("Failed to submit onchain send: {e}")))?;

    let mut events = state.client.subscribe_operation_events(operation);
    while let Some(entry) = events.next().await {
        if let Some(e) = entry.to_event::<SendSuccessEvent>() {
            return Ok(Json(FederationWalletSendResponse { txid: e.txid }));
        }
        if let Some(e) = entry.to_event::<TxRejectEvent>() {
            return Err(CliError::bad_request(format!(
                "Transaction rejected: {}",
                e.error
            )));
        }
        if entry.to_event::<SendFailureEvent>().is_some() {
            return Err(CliError::internal(
                "Failure to retrieve txid from federation",
            ));
        }
    }
    Err(CliError::internal("Event stream ended unexpectedly"))
}

/// Generate deposit address for a federation
#[instrument(skip_all, err)]
async fn federation_module_wallet_receive(
    State(state): State<AppState>,
    Json(payload): Json<FederationWalletReceiveRequest>,
) -> Result<Json<FederationWalletReceiveResponse>, CliError> {
    let federation = resolve_federation(&state, payload.federation)?;

    let address = state
        .client
        .wallet_deposit_address(federation, GATEWAY_ACCOUNT)
        .map_err(CliError::internal)?;

    Ok(Json(FederationWalletReceiveResponse {
        address: address.as_unchecked().clone(),
    }))
}
