pub mod audit;
pub mod bitcoin;
pub mod config;
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
use maud::html;

use picomint_sqlite::DbRead;

use crate::consensus::api::ConsensusApi;
use crate::consensus::db::ExpiryStatusTable;
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
    let audit_summary = api.federation_audit();
    let bitcoin_rpc_status = api.server.btc_rpc.status();

    // One read snapshot for the whole page, so every value rendered below
    // reflects the same database state.
    let dbtx = api.server.db.begin_read();

    let expiry_status = dbtx.get(&ExpiryStatusTable, &());

    let content = html! {
        div class="row gy-4" {
            div class="col-12" {
                (general::render(&federation_name, &guardian_names, &p2p_connection_status))
            }
        }

        div class="row gy-4 mt-2" {
            div class="col-lg-6" {
                (invite::render(crate::consensus::wallet::consensus_block_count(&api.server, &dbtx)))
            }

            div class="col-lg-6" {
                (ln::render(&api.server, &dbtx))
            }
        }

        div class="row gy-4 mt-2" {
            div class="col-lg-6" {
                (bitcoin::render(&bitcoin_rpc_status))
            }

            div class="col-lg-6" {
                (audit::render(&audit_summary))
            }
        }

        div class="row gy-4 mt-2" {
            div class="col-12" {
                (wallet::render(&api.server, &dbtx))
            }
        }

        div class="row gy-4 mt-2" {
            div class="col-lg-6" {
                (config::render())
            }

            div class="col-lg-6" {
                (expiry::render(expiry_status.as_ref()))
            }
        }
    };

    Html(dashboard_layout(content, env!("CARGO_PKG_VERSION")).into_string()).into_response()
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
