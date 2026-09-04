pub mod actions;
pub mod bitcoin;
pub mod expiry;
pub mod general;
pub mod invite;
pub mod modules;

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use maud::{Markup, html};

use picomint_redb::DbRead;

use crate::consensus::api::ConsensusApi;
use crate::consensus::db::{ExpiryStatusTable, consensus_block_count, consensus_version};
use crate::consensus::engine::get_finished_session_count;
use crate::ui::assets::WithStaticRoutesExt;
use crate::ui::dashboard::modules::{ln, wallet};
use crate::ui::{ROOT_ROUTE, dashboard_layout};

pub const BACKUP_CONFIG_ROUTE: &str = "/backup-config";
pub const SET_EXPIRY_ROUTE: &str = "/expiry/set";
pub const CLEAR_EXPIRY_ROUTE: &str = "/expiry/clear";

async fn backup_config(State(state): State<Arc<ConsensusApi>>) -> impl IntoResponse {
    let body = serde_json::to_vec_pretty(&state.server.cfg).expect("ServerConfig is serializable");

    (
        [
            ("Content-Type", "application/json"),
            (
                "Content-Disposition",
                "attachment; filename=\"config.json\"",
            ),
        ],
        body,
    )
}

fn tile(label: &str, value: Markup) -> Markup {
    html! {
        div class="tile" {
            span class="tile-label" { (label) }
            span class="tile-value" { (value) }
        }
    }
}

/// Renders one key-value row of a status card.
pub fn kv(label: &str, value: Markup) -> Markup {
    html! {
        div class="kv" {
            span class="kv-label" { (label) }
            span class="kv-value mono" { (value) }
        }
    }
}

async fn dashboard_view(State(state): State<Arc<ConsensusApi>>) -> impl IntoResponse {
    let api = &*state;

    let guardian_names: BTreeMap<_, _> = api
        .server
        .cfg
        .consensus
        .peers
        .iter()
        .map(|(peer, endpoint)| (*peer, endpoint.name.clone()))
        .collect();
    let federation_name = api.server.cfg.consensus.name.clone();
    let p2p_connection_status: BTreeMap<_, _> = api
        .p2p_status_receivers
        .iter()
        .map(|(peer, receiver)| (*peer, receiver.borrow().clone()))
        .collect();
    let bitcoin_rpc_status = api.server.btc_rpc.status();

    // One read snapshot for the whole page, so every value rendered below
    // reflects the same database state.
    let dbtx = api.server.db.begin_read();

    let expiry_status = dbtx.get(&ExpiryStatusTable, &());

    let session_count = get_finished_session_count(&dbtx);
    let block_count = consensus_block_count(&api.server, &dbtx);
    let version = consensus_version(&api.server, &dbtx);

    let value_in_custody = crate::consensus::wallet::federation_wallet(&dbtx)
        .map(|wallet| wallet.value.to_btc())
        .unwrap_or(0.0);

    let content = html! {
        div class="tiles" {
            (tile("Value in Custody", html! {
                (format!("{value_in_custody:.8}")) " " span class="tile-unit" { "BTC" }
            }))
            (tile("Block Count", html! { (block_count) }))
            (tile("Session Count", html! { (session_count) }))
            (tile("Consensus Version", html! { (version) }))
        }

        div class="grid" {
            div class="grid-col" {
                (general::render(&federation_name, &guardian_names, &p2p_connection_status))
                (wallet::render_pending(&api.server, &dbtx))
            }

            div class="grid-col" {
                (bitcoin::render(&bitcoin_rpc_status))
                (wallet::render(&api.server, &dbtx))
                (ln::render(&dbtx))
            }
        }

        (actions::render(&api.server, &dbtx, expiry_status.as_ref()))
        (invite::render(block_count))
    };

    Html(dashboard_layout(&federation_name, env!("CARGO_PKG_VERSION"), content).into_string())
        .into_response()
}

pub fn router(api: Arc<ConsensusApi>) -> Router {
    Router::new()
        .route(ROOT_ROUTE, get(dashboard_view))
        .route(BACKUP_CONFIG_ROUTE, get(backup_config))
        .route(SET_EXPIRY_ROUTE, post(expiry::post_set))
        .route(CLEAR_EXPIRY_ROUTE, post(expiry::post_clear))
        .route(
            invite::INVITE_CREATE_ROUTE,
            post(invite::post_create_invite),
        )
        .route(ln::LN_ADD_ROUTE, post(ln::post_add))
        .route(ln::LN_REMOVE_ROUTE, post(ln::post_remove))
        .with_static_routes()
        .with_state(api)
}
