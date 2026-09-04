//! Loading-screen-only router served while DKG is running.
//!
//! Mounted on `UI_ADDR` between the setup and consensus phases. Every path
//! falls through to a single 503 response that renders the same waiting
//! page the operator saw the moment they clicked "Start DKG". The 503
//! status is load-bearing: the polling JS embedded in the page redirects
//! to `/` on status `200`, so any other status keeps the waiting page up. Once
//! `NodeConfig` is committed and the consensus UI binds the same port,
//! `/` starts returning 200 and the redirect fires.

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use maud::{Markup, html};
use picomint_redb::{Database, DbRead};

use crate::config::db::DkgParamsTable;
use crate::config::setup::NodeSetupCode;
use crate::ui::assets::WithStaticRoutesExt;
use crate::ui::{ROOT_ROUTE, copiable_text, single_card_layout};

/// Shared content used by both this router's fallback and the setup UI's
/// post-`start_dkg` response, so the operator's screen is identical whether
/// they just clicked the button or reopened the tab after a restart.
/// `setup_code` is this node's `NodeSetupCode`.
pub fn loading_card(setup_code: &NodeSetupCode) -> Markup {
    let content = html! {
        span { "Share with nodes who still need it." }

        (copiable_text(&picomint_base32::encode(setup_code)))

        div class="alert alert-info" {
            "All nodes need to confirm and start the DKG. Once the DKG is complete you will be redirected to the dashboard."
        }

        div
            hx-get=(ROOT_ROUTE)
            hx-trigger="every 2s"
            hx-swap="none"
            hx-on--after-request={
                "if (event.detail.xhr.status === 200) { window.location.href = '" (ROOT_ROUTE) "'; }"
            }
            style="display: none;"
        {}
    };

    single_card_layout("Generating Keys...", content)
}

async fn loading_page(State(db): State<Database>) -> Response {
    // `store_node_config` clears the table moments before this router is
    // aborted, so a request can land after DKG has completed. A bare 503
    // keeps the polling page up until the consensus UI takes over the port.
    let Some(params) = db.begin_read().get(&DkgParamsTable, &()) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };

    let node = params
        .nodes
        .get(&params.identity)
        .expect("our node id is always in the node map");

    (
        StatusCode::SERVICE_UNAVAILABLE,
        Html(loading_card(node).into_string()),
    )
        .into_response()
}

pub fn router(db: Database) -> Router {
    Router::new()
        .fallback(loading_page)
        .with_static_routes()
        .with_state(db)
}
