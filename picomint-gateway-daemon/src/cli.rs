use std::str::FromStr;
use std::time::Duration;

use axum::Router;
use axum::extract::{Json, State};
use axum::routing::post;
use bitcoin::FeeRate;
use hex::ToHex;
use ldk_node::lightning::ln::msgs::SocketAddress;
use ldk_node::lightning::routing::gossip::NodeId;
use ldk_node::payment::{PaymentKind, PaymentStatus};
use ldk_node::{PendingSweepBalance, UserChannelId};
use lightning_invoice::{Bolt11InvoiceDescription as LdkBolt11InvoiceDescription, Description};
use picomint_cli_server::{CliError, serve};
use picomint_core::lightning::gateway::GatewayPk;
use picomint_gateway_cli_core::{
    ChannelInfo, ClientAddRequest, ClientBalanceRequest, ClientBalanceResponse,
    ClientConfigRequest, ClientConfigResponse, ClientEcashCountRequest, ClientEcashCountResponse,
    ClientEcashReceiveRequest, ClientEcashReceiveResponse, ClientEcashSendRequest,
    ClientEcashSendResponse, ClientListResponse, ClientOnchainReceiveRequest,
    ClientOnchainReceiveResponse, ClientOnchainSendFeeRequest, ClientOnchainSendFeeResponse,
    ClientOnchainSendRequest, ClientOnchainSendResponse, ClientRemoveRequest, InfoResponse,
    LdkBalancesResponse, LdkChannelCloseRequest, LdkChannelListResponse, LdkChannelOpenRequest,
    LdkChannelSpliceInRequest, LdkChannelSpliceOutRequest, LdkLightningProbeRequest,
    LdkLightningReceiveRequest, LdkLightningReceiveResponse, LdkLightningSendRequest,
    LdkLightningSendResponse, LdkOnchainReceiveResponse, LdkOnchainSendRequest,
    LdkOnchainSendResponse, LdkPeerConnectRequest, LdkPeerDisconnectRequest, LdkPeerListResponse,
    MnemonicResponse, PeerInfo, QueryRequest, QueryResponse, ROUTE_CLIENT_ADD,
    ROUTE_CLIENT_BALANCE, ROUTE_CLIENT_CONFIG, ROUTE_CLIENT_ECASH_COUNT,
    ROUTE_CLIENT_ECASH_RECEIVE, ROUTE_CLIENT_ECASH_SEND, ROUTE_CLIENT_LIST,
    ROUTE_CLIENT_ONCHAIN_RECEIVE, ROUTE_CLIENT_ONCHAIN_SEND, ROUTE_CLIENT_ONCHAIN_SEND_FEE,
    ROUTE_CLIENT_REMOVE, ROUTE_INFO, ROUTE_LDK_BALANCES, ROUTE_LDK_CHANNEL_CLOSE,
    ROUTE_LDK_CHANNEL_LIST, ROUTE_LDK_CHANNEL_OPEN, ROUTE_LDK_CHANNEL_SPLICE_IN,
    ROUTE_LDK_CHANNEL_SPLICE_OUT, ROUTE_LDK_LIGHTNING_PROBE, ROUTE_LDK_LIGHTNING_RECEIVE,
    ROUTE_LDK_LIGHTNING_SEND, ROUTE_LDK_ONCHAIN_RECEIVE, ROUTE_LDK_ONCHAIN_SEND,
    ROUTE_LDK_PEER_CONNECT, ROUTE_LDK_PEER_DISCONNECT, ROUTE_LDK_PEER_LIST, ROUTE_MNEMONIC,
    ROUTE_QUERY,
};
use tower_http::cors::CorsLayer;
use tracing::{info, instrument};

use crate::AppState;

pub async fn run(state: AppState) {
    let data_dir = state.data_dir.clone();

    let router = router().with_state(state).layer(CorsLayer::permissive());

    serve(&data_dir, router).await;
}

fn router() -> Router<AppState> {
    Router::new()
        // Top-level
        .route(ROUTE_INFO, post(info))
        .route(ROUTE_MNEMONIC, post(mnemonic))
        .route(ROUTE_QUERY, post(query))
        // LDK node management
        .route(ROUTE_LDK_BALANCES, post(ldk_balances))
        .route(ROUTE_LDK_CHANNEL_OPEN, post(ldk_channel_open))
        .route(ROUTE_LDK_CHANNEL_CLOSE, post(ldk_channel_close))
        .route(ROUTE_LDK_CHANNEL_LIST, post(ldk_channel_list))
        .route(ROUTE_LDK_CHANNEL_SPLICE_IN, post(ldk_channel_splice_in))
        .route(ROUTE_LDK_CHANNEL_SPLICE_OUT, post(ldk_channel_splice_out))
        .route(ROUTE_LDK_ONCHAIN_RECEIVE, post(ldk_onchain_receive))
        .route(ROUTE_LDK_ONCHAIN_SEND, post(ldk_onchain_send))
        .route(ROUTE_LDK_LIGHTNING_RECEIVE, post(ldk_lightning_receive))
        .route(ROUTE_LDK_LIGHTNING_SEND, post(ldk_lightning_send))
        .route(ROUTE_LDK_LIGHTNING_PROBE, post(ldk_lightning_probe))
        .route(ROUTE_LDK_PEER_CONNECT, post(ldk_peer_connect))
        .route(ROUTE_LDK_PEER_DISCONNECT, post(ldk_peer_disconnect))
        .route(ROUTE_LDK_PEER_LIST, post(ldk_peer_list))
        // Mint management
        .route(ROUTE_CLIENT_ADD, post(client_add))
        .route(ROUTE_CLIENT_REMOVE, post(client_remove))
        .route(ROUTE_CLIENT_LIST, post(client_list))
        .route(ROUTE_CLIENT_CONFIG, post(client_config))
        .route(ROUTE_CLIENT_BALANCE, post(client_balance))
        // Per-mint module commands
        .route(ROUTE_CLIENT_ECASH_COUNT, post(client_ecash_count))
        .route(ROUTE_CLIENT_ECASH_SEND, post(client_ecash_send))
        .route(ROUTE_CLIENT_ECASH_RECEIVE, post(client_ecash_receive))
        .route(ROUTE_CLIENT_ONCHAIN_SEND_FEE, post(client_onchain_send_fee))
        .route(ROUTE_CLIENT_ONCHAIN_SEND, post(client_onchain_send))
        .route(ROUTE_CLIENT_ONCHAIN_RECEIVE, post(client_onchain_receive))
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

async fn query(
    State(state): State<AppState>,
    Json(request): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, CliError> {
    let rows = tokio::task::spawn_blocking(move || {
        picomint_analytics::query(&state.data_dir, &request.query)
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
async fn ldk_lightning_receive(
    State(state): State<AppState>,
    Json(payload): Json<LdkLightningReceiveRequest>,
) -> Result<Json<LdkLightningReceiveResponse>, CliError> {
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

    Ok(Json(LdkLightningReceiveResponse {
        invoice: invoice.to_string(),
    }))
}

/// Pays an outgoing LN invoice using the gateway's own funds
#[instrument(skip_all, err)]
async fn ldk_lightning_send(
    State(state): State<AppState>,
    Json(payload): Json<LdkLightningSendRequest>,
) -> Result<Json<LdkLightningSendResponse>, CliError> {
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

    Ok(Json(LdkLightningSendResponse {
        preimage: preimage.encode_hex::<String>(),
    }))
}

/// Sends payment probes over all routes towards a node for the given amount,
/// to exercise pathfinding and warm the scorer without moving funds. Probe
/// outcomes surface only in the daemon's LDK logs, so nothing meaningful is
/// returned here.
#[instrument(skip_all, err)]
async fn ldk_lightning_probe(
    State(state): State<AppState>,
    Json(payload): Json<LdkLightningProbeRequest>,
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
// Mint management handlers
// ---------------------------------------------------------------------------

/// Add a new mint
#[instrument(skip_all, err)]
async fn client_add(
    State(state): State<AppState>,
    Json(payload): Json<ClientAddRequest>,
) -> Result<Json<()>, CliError> {
    state
        .client
        .add_mint(&payload.invite, Some(state.network))
        .await?;

    Ok(Json(()))
}

/// Remove a mint: shut its client runtime down, then delete its
/// client rows and the daemon's contract rows in one dbtx. Destructive —
/// in-flight contracts are dropped with their rows, so the operator
/// checks for in-flight payments via the query route before removing;
/// failing to check might result in loss of funds.
#[instrument(skip_all, err)]
async fn client_remove(
    State(state): State<AppState>,
    Json(payload): Json<ClientRemoveRequest>,
) -> Result<Json<()>, CliError> {
    let dbtx = state.client.begin_remove_mint(payload.mint).await?;

    crate::db::wipe_mint_rows(&dbtx, payload.mint);

    dbtx.commit();

    info!(mint = %payload.mint, "Removed mint");

    Ok(Json(()))
}

/// List connected mints
#[instrument(skip_all, err)]
async fn client_list(State(state): State<AppState>) -> Result<Json<ClientListResponse>, CliError> {
    Ok(Json(ClientListResponse {
        mints: state.mint_list(),
    }))
}

/// Display mint config
#[instrument(skip_all, err)]
async fn client_config(
    State(state): State<AppState>,
    Json(payload): Json<ClientConfigRequest>,
) -> Result<Json<ClientConfigResponse>, CliError> {
    let mint = payload.mint;

    let config = state
        .client
        .config(mint)
        .ok_or_else(|| CliError::bad_request("Mint not added"))?;

    Ok(Json(ClientConfigResponse {
        config: serde_json::to_value(config).expect("ConsensusConfig is serializable"),
    }))
}

/// Get a mint's ecash balance
#[instrument(skip_all, err)]
async fn client_balance(
    State(state): State<AppState>,
    Json(payload): Json<ClientBalanceRequest>,
) -> Result<Json<ClientBalanceResponse>, CliError> {
    let mint = payload.mint;

    let balance_msat = state.client.ecash_balance(mint, payload.account);

    Ok(Json(ClientBalanceResponse { balance_msat }))
}

// ---------------------------------------------------------------------------
// Per-mint module handlers
// ---------------------------------------------------------------------------

/// Count held ecash notes by denomination
#[instrument(skip_all, err)]
async fn client_ecash_count(
    State(state): State<AppState>,
    Json(payload): Json<ClientEcashCountRequest>,
) -> Result<Json<ClientEcashCountResponse>, CliError> {
    let mint = payload.mint;
    let counts = state.client.ecash_count(mint, payload.account);
    Ok(Json(ClientEcashCountResponse { counts }))
}

/// Spend ecash from a mint
#[instrument(skip_all, err)]
async fn client_ecash_send(
    State(state): State<AppState>,
    Json(payload): Json<ClientEcashSendRequest>,
) -> Result<Json<ClientEcashSendResponse>, CliError> {
    let mint = payload.mint;

    let ecash = state
        .client
        .ecash_send(
            mint,
            payload.account,
            picomint_core::Amount::from_sat(payload.amount.to_sat()),
        )
        .await
        .map_err(CliError::internal)?;

    Ok(Json(ClientEcashSendResponse { ecash }))
}

/// Reissue an ecash string into the named account. Returns the operation
/// id; acceptance shows up in the analytics as `core_tx_accept`.
#[instrument(skip_all, err)]
async fn client_ecash_receive(
    State(state): State<AppState>,
    Json(payload): Json<ClientEcashReceiveRequest>,
) -> Result<Json<ClientEcashReceiveResponse>, CliError> {
    let operation = state
        .client
        .ecash_receive(payload.mint, payload.account, &payload.ecash)
        .map_err(|e| CliError::internal(format!("Failed to submit reissue: {e}")))?;

    Ok(Json(ClientEcashReceiveResponse { operation }))
}

/// Fetch the current onchain send-fee for a mint
#[instrument(skip_all, err)]
async fn client_onchain_send_fee(
    State(state): State<AppState>,
    Json(payload): Json<ClientOnchainSendFeeRequest>,
) -> Result<Json<ClientOnchainSendFeeResponse>, CliError> {
    let mint = payload.mint;
    let fee = state
        .client
        .onchain_send_fee(mint)
        .await
        .map_err(|e| CliError::internal(format!("Failed to fetch send fee: {e}")))?;
    Ok(Json(ClientOnchainSendFeeResponse { fee }))
}

/// Withdraw onchain from a mint. Blocks until the send reaches a
/// terminal state: confirmed broadcast, mint rejected the input tx, or
/// the mint accepted but never produced a bitcoin txid.
#[instrument(skip_all, err)]
async fn client_onchain_send(
    State(state): State<AppState>,
    Json(payload): Json<ClientOnchainSendRequest>,
) -> Result<Json<ClientOnchainSendResponse>, CliError> {
    let mint = payload.mint;
    let operation = state
        .client
        .onchain_send(
            mint,
            payload.account,
            payload.address,
            payload.amount,
            payload.fee,
        )
        .await
        .map_err(|e| CliError::internal(format!("Failed to submit onchain send: {e}")))?;

    Ok(Json(ClientOnchainSendResponse { operation }))
}

/// Generate deposit address for a mint
#[instrument(skip_all, err)]
async fn client_onchain_receive(
    State(state): State<AppState>,
    Json(payload): Json<ClientOnchainReceiveRequest>,
) -> Result<Json<ClientOnchainReceiveResponse>, CliError> {
    let mint = payload.mint;

    let address = state
        .client
        .onchain_receive(mint, payload.account)
        .map_err(CliError::internal)?;

    Ok(Json(ClientOnchainReceiveResponse {
        address: address.as_unchecked().clone(),
    }))
}
