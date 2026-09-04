use std::sync::Arc;

use axum::Router;
use axum::extract::{Multipart, State};
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::{get, post};
use axum_extra::extract::Form;
use maud::{Markup, html};
use serde::Deserialize;

use crate::config::ServerConfig;
use crate::config::setup::SetupApi;
use crate::ui::assets::WithStaticRoutesExt;
use crate::ui::{ROOT_ROUTE, copiable_text, single_card_layout};

// Setup route constants
pub const MINT_SETUP_ROUTE: &str = "/mint-setup";
pub const ADD_SETUP_CODE_ROUTE: &str = "/add-setup-code";
pub const RESET_SETUP_CODES_ROUTE: &str = "/reset-setup-codes";
pub const START_DKG_ROUTE: &str = "/start-dkg";
pub const RESTORE_CONFIG_ROUTE: &str = "/restore-config";
pub const RESTORE_PAGE_ROUTE: &str = "/restore";

#[derive(Debug, Deserialize)]
pub(crate) struct SetupInput {
    pub name: String,
    #[serde(default)]
    pub is_lead: bool,
    pub mint_name: String,
    #[serde(default)]
    pub mint_size: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PeerInfoInput {
    pub peer_info: String,
}

fn peer_list_section(
    connected_peers: &[String],
    mint_size: Option<u8>,
    cfg_mint_name: &Option<String>,
    error: Option<&str>,
) -> Markup {
    let total_guardians = connected_peers.len() + 1;
    let can_start_dkg =
        mint_size.is_some_and(|expected| total_guardians == expected as usize);

    html! {
        div id="peer-list-section" {
            @if let Some(expected) = mint_size {
                span { (format!("{total_guardians} of {expected} guardians connected.")) }
            } @else {
                span { "Add setup code for every other guardian." }
            }

            @if !connected_peers.is_empty() {
                div class="list list-bordered" {
                    @for peer in connected_peers {
                        div class="list-row" {
                            span class="list-row-name" { (peer) }
                        }
                    }
                }

                form id="reset-form" method="post" action=(RESET_SETUP_CODES_ROUTE) hidden {}
                div style="display: flex; justify-content: center" {
                    button type="button" class="btn btn-danger" style="border: none" onclick="if(confirm('Are you sure you want to reset all guardians?')){document.getElementById('reset-form').submit();}" {
                        "Reset Guardians"
                    }
                }
            }

            @if can_start_dkg {
                @let has_settings = cfg_mint_name.is_some() || mint_size.is_some();

                form id="start-dkg-form" class="form-stack" hx-post=(START_DKG_ROUTE) hx-target="#peer-list-section" hx-swap="outerHTML" {
                    @if let Some(error) = error {
                        div class="alert alert-danger" { (error) }
                    }
                    button type="submit" class="btn btn-primary btn-lg btn-block" { "Confirm" }
                }

                @if has_settings {
                    span class="hint" {
                        @if let Some(name) = cfg_mint_name {
                            (name) " mint has been configured"
                        } @else {
                            "The mint has been configured"
                        }
                        "."
                    }
                }
            } @else {
                form id="add-setup-code-form" class="form-stack" hx-post=(ADD_SETUP_CODE_ROUTE) hx-target="#peer-list-section" hx-swap="outerHTML" {
                    input type="text" id="peer_info" name="peer_info"
                        placeholder="Paste Setup Code" required;

                    @if let Some(error) = error {
                        div class="alert alert-danger" { (error) }
                    }
                    button type="submit" class="btn btn-primary btn-lg btn-block" { "Add Guardian" }
                }
            }
        }
    }
}

fn restore_form_content(error: Option<&str>) -> Markup {
    html! {
        form id="restore-form"
            class="form-stack"
            hx-post=(RESTORE_CONFIG_ROUTE)
            hx-encoding="multipart/form-data"
            hx-target="#restore-form"
            hx-swap="outerHTML"
        {
            div class="alert alert-info" {
                "Upload your saved server config to restore."
            }

            input type="file" id="config_file" name="config_file"
                accept="application/json" required;

            @if let Some(error) = error {
                div class="alert alert-danger" { (error) }
            }

            button type="submit" class="btn btn-outline btn-lg btn-block" { "Restore from Config" }
        }
    }
}

fn setup_form_content(error: Option<&str>) -> Markup {
    html! {
        form id="setup-form" class="form-stack" hx-post=(ROOT_ROUTE) hx-target="#setup-form" hx-swap="outerHTML" {
            input type="text" id="name" name="name" placeholder="Your Guardian Name" required;

            div class="alert alert-warning" {
                "Exactly one guardian must set the global config."
            }

            div class="inset-panel" {
                div class="check-row" {
                    input type="checkbox" class="toggle-control" id="is_lead" name="is_lead" value="true";

                    label for="is_lead" {
                        "Set the global config"
                    }
                }

                div class="toggle-content form-stack" {
                    input type="text" id="mint_name" name="mint_name" placeholder="Mint Name";

                    div class="field" {
                        span class="field-label" {
                            "Total number of guardians (including you)"
                        }
                        div class="pill-group" {
                            @for size in [4u32, 7, 10, 13, 16, 19] {
                                // `required` is intentionally omitted: the
                                // radios are hidden when `is_lead` is off, and
                                // browsers refuse to focus a hidden required
                                // control — they silently block submit even
                                // for non-leader guardians. The server
                                // re-validates that a leader supplied a size.
                                input type="radio"
                                    id=(format!("mint_size_{size}"))
                                    name="mint_size"
                                    value=(size.to_string());
                                label class="pill" for=(format!("mint_size_{size}")) {
                                    (size.to_string())
                                }
                            }
                        }
                    }
                }
            }

            @if let Some(error) = error {
                div class="alert alert-danger" { (error) }
            }
            button type="submit" class="btn btn-primary btn-lg btn-block" { "Confirm" }

            div style="display: flex; justify-content: center" {
                a href=(RESTORE_PAGE_ROUTE) {
                    "Restore from Config"
                }
            }
        }
    }
}

// GET handler for the /setup route (display the setup form)
async fn setup_form(State(state): State<Arc<SetupApi>>) -> impl IntoResponse {
    if state.setup_code().await.is_some() {
        return Redirect::to(MINT_SETUP_ROUTE).into_response();
    }

    Html(single_card_layout("Guardian Setup", setup_form_content(None)).into_string())
        .into_response()
}

// GET handler for the /restore route (dedicated page for restoring from a
// previously-saved server config).
async fn restore_page(State(state): State<Arc<SetupApi>>) -> impl IntoResponse {
    if state.setup_code().await.is_some() {
        return Redirect::to(MINT_SETUP_ROUTE).into_response();
    }

    Html(single_card_layout("Restore from Config", restore_form_content(None)).into_string())
        .into_response()
}

// POST handler for the /setup route (process the setup form)
async fn setup_submit(
    State(state): State<Arc<SetupApi>>,
    Form(input): Form<SetupInput>,
) -> impl IntoResponse {
    // Only use these settings if is_lead is true
    let mint_name = if input.is_lead {
        Some(input.mint_name)
    } else {
        None
    };

    let mint_size = if input.is_lead {
        let s = input.mint_size.trim();
        if s.is_empty() {
            None
        } else {
            match s.parse::<u8>() {
                Ok(size) => Some(size),
                Err(_) => {
                    return Html(setup_form_content(Some("Invalid mint size")).into_string())
                        .into_response();
                }
            }
        }
    } else {
        None
    };

    match state
        .set_local_parameters(input.name, mint_name, mint_size)
        .await
    {
        Ok(_) => (
            [("HX-Redirect", MINT_SETUP_ROUTE)],
            Html(String::new()),
        )
            .into_response(),
        Err(e) => Html(setup_form_content(Some(&e.to_string())).into_string()).into_response(),
    }
}

// GET handler for the /mint-setup route (main mint management page)
async fn mint_setup(State(state): State<Arc<SetupApi>>) -> impl IntoResponse {
    // If the user lands here too early (before local parameters have been
    // set), send them back to /setup to fill in their guardian params first.
    let Some(our_setup_code) = state.setup_code().await else {
        return Redirect::to(ROOT_ROUTE).into_response();
    };

    let our_connection_info = picomint_base32::encode(&our_setup_code);

    let connected_peers = state.connected_peers().await;
    let mint_size = state.mint_size().await;
    let cfg_mint_name = state.cfg_mint_name().await;

    let content = html! {
        span { "Share this with your fellow guardians." }

        (copiable_text(&our_connection_info))

        (peer_list_section(&connected_peers, mint_size, &cfg_mint_name, None))
    };

    Html(single_card_layout("Mint Setup", content).into_string()).into_response()
}

async fn post_add_setup_code(
    State(state): State<Arc<SetupApi>>,
    Form(input): Form<PeerInfoInput>,
) -> impl IntoResponse {
    let error = state.add_peer_setup_code(input.peer_info).await.err();

    let connected_peers = state.connected_peers().await;
    let mint_size = state.mint_size().await;
    let cfg_mint_name = state.cfg_mint_name().await;

    Html(
        peer_list_section(
            &connected_peers,
            mint_size,
            &cfg_mint_name,
            error
                .as_ref()
                .map(std::string::ToString::to_string)
                .as_deref(),
        )
        .into_string(),
    )
    .into_response()
}

async fn post_start_dkg(State(state): State<Arc<SetupApi>>) -> impl IntoResponse {
    match state.start_dkg().await {
        Ok(()) => {
            let code = state
                .setup_code()
                .await
                .expect("setup_code is always set once start_dkg has succeeded");

            (
                [("HX-Retarget", "body"), ("HX-Reswap", "innerHTML")],
                Html(crate::ui::dkg::loading_card(&code).into_string()),
            )
                .into_response()
        }
        Err(e) => {
            let connected_peers = state.connected_peers().await;
            let mint_size = state.mint_size().await;
            let cfg_mint_name = state.cfg_mint_name().await;

            Html(
                peer_list_section(
                    &connected_peers,
                    mint_size,
                    &cfg_mint_name,
                    Some(&e.to_string()),
                )
                .into_string(),
            )
            .into_response()
        }
    }
}

async fn post_restore_config(
    State(state): State<Arc<SetupApi>>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let bytes = match multipart.next_field().await {
        Ok(Some(field)) => match field.bytes().await {
            Ok(b) => b,
            Err(e) => {
                return Html(
                    restore_form_content(Some(&format!("Read failed: {e}"))).into_string(),
                )
                .into_response();
            }
        },
        Ok(None) => {
            return Html(restore_form_content(Some("No file uploaded")).into_string())
                .into_response();
        }
        Err(e) => {
            return Html(restore_form_content(Some(&format!("Upload failed: {e}"))).into_string())
                .into_response();
        }
    };

    let cfg: ServerConfig = match serde_json::from_slice(&bytes) {
        Ok(c) => c,
        Err(e) => {
            return Html(
                restore_form_content(Some(&format!("Invalid config JSON: {e}"))).into_string(),
            )
            .into_response();
        }
    };

    if let Err(e) = state.restore_config(cfg).await {
        return Html(restore_form_content(Some(&e.to_string())).into_string()).into_response();
    }

    let waiting = html! {
        div class="alert alert-info" {
            "Config restored. The guardian is rejoining the mint — you'll be redirected once it's back online."
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

        div class="spinner" {}

        span class="hint" style="text-align: center" { "Waiting for guardian to come online..." }
    };

    (
        [("HX-Retarget", "body"), ("HX-Reswap", "innerHTML")],
        Html(single_card_layout("Restoring Config", waiting).into_string()),
    )
        .into_response()
}

async fn post_reset_setup_codes(State(state): State<Arc<SetupApi>>) -> impl IntoResponse {
    state.reset_setup_codes().await;

    Redirect::to(MINT_SETUP_ROUTE).into_response()
}

pub fn router(api: Arc<SetupApi>) -> Router {
    Router::new()
        .route(ROOT_ROUTE, get(setup_form).post(setup_submit))
        .route(MINT_SETUP_ROUTE, get(mint_setup))
        .route(ADD_SETUP_CODE_ROUTE, post(post_add_setup_code))
        .route(RESET_SETUP_CODES_ROUTE, post(post_reset_setup_codes))
        .route(START_DKG_ROUTE, post(post_start_dkg))
        .route(RESTORE_CONFIG_ROUTE, post(post_restore_config))
        .route(RESTORE_PAGE_ROUTE, get(restore_page))
        .with_static_routes()
        .with_state(api)
}
