//! Loading-screen-only router served while DKG is running.
//!
//! Mounted on `UI_ADDR` between the setup and consensus phases. Every path
//! falls through to a single 503 response that renders the same waiting
//! page the operator saw the moment they clicked "Start DKG". The 503
//! status is load-bearing: the polling JS embedded in the page redirects
//! to `/` on status `200`, so any other status keeps the spinner up. Once
//! `ServerConfig` is committed and the consensus UI binds the same port,
//! `/` starts returning 200 and the redirect fires.

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use maud::{Markup, html};
use picomint_redb::Database;

use crate::config::db::ConfigGenParamsTable;
use crate::config::setup::PeerSetupCode;
use crate::ui::assets::WithStaticRoutesExt;
use crate::ui::{ROOT_ROUTE, copiable_text, single_card_layout};

/// Shared content used by both this router's fallback and the setup UI's
/// post-`start_dkg` response, so the operator's screen is identical whether
/// they just clicked the button or reopened the tab after a restart.
/// `setup_code` is this guardian's `PeerSetupCode` — always available
/// because we only enter this phase once `ConfigGenParams` has been
/// persisted (or, on the setup-UI side, after `start_dkg` has succeeded in
/// the same process).
pub fn loading_card(setup_code: &PeerSetupCode) -> Markup {
    let content = html! {
        p { "Share with guardians who still need it." }

        div class="mb-4" {
            (copiable_text(&picomint_base32::encode(setup_code)))
        }

        div class="alert alert-info mb-3" {
            "All guardians need to confirm and start the DKG. Once the DKG is complete you will be redirected to the dashboard."
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

        div class="text-center mt-4" {
            div class="spinner-border text-primary" role="status" {
                span class="visually-hidden" { "Loading..." }
            }
        }
    };

    single_card_layout("DKG Started", content)
}

async fn loading_page(State(db): State<Database>) -> impl IntoResponse {
    let params = db
        .begin_read()
        .get(&ConfigGenParamsTable, &())
        .expect("DKG UI only runs while ConfigGenParams is persisted");

    let peer = params
        .peers
        .get(&params.identity)
        .expect("our peer id is always in the peer map");

    (
        StatusCode::SERVICE_UNAVAILABLE,
        Html(loading_card(peer).into_string()),
    )
}

pub fn router(db: Database) -> Router {
    Router::new()
        .fallback(loading_page)
        .with_static_routes()
        .with_state(db)
}
