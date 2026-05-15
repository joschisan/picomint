pub mod audit;
pub mod bitcoin;
pub mod config;
pub mod expiry;
pub mod general;
pub mod invite;
pub mod modules;
pub mod peers;

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use maud::html;

use crate::consensus::api::ConsensusApi;
use crate::ui::assets::WithStaticRoutesExt;
use crate::ui::dashboard::modules::{ln, mint, wallet};
use crate::ui::{ROOT_ROUTE, dashboard_layout};

pub const BACKUP_CONFIG_ROUTE: &str = "/backup-config";
pub const SET_EXPIRY_ROUTE: &str = "/expiry/set";
pub const CLEAR_EXPIRY_ROUTE: &str = "/expiry/clear";

async fn backup_config(State(state): State<Arc<ConsensusApi>>) -> impl IntoResponse {
    let body = serde_json::to_vec_pretty(&state.cfg).expect("ServerConfig is serializable");

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

    let guardian_names: std::collections::BTreeMap<_, _> = api
        .cfg
        .consensus
        .peers
        .iter()
        .map(|(peer, endpoint)| (*peer, endpoint.name.clone()))
        .collect();
    let federation_name = api.cfg.consensus.name.clone();
    let session_count = api.session_count().await;
    let p2p_connection_status: std::collections::BTreeMap<_, _> = api
        .p2p_status_receivers
        .iter()
        .map(|(peer, receiver)| (*peer, receiver.borrow().clone()))
        .collect();
    let invite_code = picomint_base32::encode(&api.cfg.get_invite_code());
    let audit_summary = api.federation_audit().await;
    let bitcoin_rpc_status = api.bitcoin_rpc_connection.status();
    let expiry_status = api.expiry_status_ui();

    let content = html! {
        div class="row gy-4" {
            div class="col-lg-6" {
                (general::render(&federation_name, session_count, &guardian_names))
            }

            div class="col-lg-6" {
                (invite::render(&invite_code, session_count))
            }
        }

        div class="row gy-4 mt-2" {
            div class="col-lg-6" {
                (config::render())
            }

            div class="col-lg-6" {
                (audit::render(&audit_summary))
            }
        }

        div class="row gy-4 mt-2" {
            div class="col-lg-6" {
                (peers::render(&p2p_connection_status))
            }

            div class="col-lg-6" {
                (expiry::render(expiry_status.as_ref()))
            }
        }

        div class="row gy-4 mt-2" {
            div class="col-12" {
                (bitcoin::render(&bitcoin_rpc_status))
            }
        }

        div class="row gy-4 mt-2" {
            div class="col-12" {
                (ln::render(&api.server.ln).await)
            }
        }

        (wallet::render(&api.server.wallet).await)

        div class="row gy-4 mt-2" {
            div class="col-12" {
                (mint::render(&api.server.mint).await)
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
        .route(ln::LN_ADD_ROUTE, post(ln::post_add))
        .route(ln::LN_REMOVE_ROUTE, post(ln::post_remove))
        .with_static_routes()
        .with_state(api)
}
