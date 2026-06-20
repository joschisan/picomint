use std::sync::Arc;

use axum::extract::{Form, State};
use axum::response::{Html, IntoResponse};
use chrono::DateTime;
use maud::{Markup, PreEscaped, html};
use picomint_guardian_cli_core::{DEFAULT_INVITE_EXPIRY_DAYS, DEFAULT_INVITE_USER_LIMIT};
use qrcode::QrCode;
use serde::Deserialize;

use crate::consensus::api::ConsensusApi;
use crate::ui::copiable_text;

pub const INVITE_CREATE_ROUTE: &str = "/invite/create";

// Card with a button that generates an invite code with the default
// expiration date and user limit. The generated code replaces the button via
// htmx.
pub fn render(session_count: u64) -> Markup {
    html! {
        div class="card h-100" {
            div class="card-header dashboard-header" { "Invite Code" }
            div class="card-body" {
                @if session_count == 0 {
                    div class="alert alert-warning" {
                        "Invite codes will be available once the federation has completed its first consensus session."
                    }
                } @else {
                    div id="invite-container" {
                        (generate_form())
                    }
                }
            }
        }
    }
}

// Form that asks the guardian for a fresh invite code with an operator-chosen
// expiration (in days) and user limit, pre-filled with the defaults. The
// generated code replaces the form via htmx.
fn generate_form() -> Markup {
    html! {
        div class="alert alert-info" {
            "Generate an invite code to onboard users to your federation."
        }

        form
            hx-post=(INVITE_CREATE_ROUTE)
            hx-target="#invite-container"
            hx-swap="innerHTML"
        {
            div class="mb-3" {
                label class="form-label" for="expiry_days" { "Expiration (days)" }
                input
                    class="form-control"
                    type="number"
                    id="expiry_days"
                    name="expiry_days"
                    min="1"
                    value=(DEFAULT_INVITE_EXPIRY_DAYS);
            }

            div class="mb-3" {
                label class="form-label" for="user_limit" { "Maximum users" }
                input
                    class="form-control"
                    type="number"
                    id="user_limit"
                    name="user_limit"
                    min="1"
                    value=(DEFAULT_INVITE_USER_LIMIT);
            }

            button class="btn btn-primary w-100 py-2" type="submit" {
                "Generate Invite Code"
            }
        }
    }
}

fn qr_code(data: &str) -> Markup {
    let qr_svg = QrCode::new(data)
        .expect("Failed to generate QR code")
        .render::<qrcode::render::svg::Color>()
        .build();

    html! {
        div class="mb-3" {
            div class="border rounded p-2 bg-white" style="width: 100%;" {
                div style="width: 100%; height: auto; overflow: hidden;" {
                    (PreEscaped(format!(r#"<div style="width: 100%; height: auto;">{}</div>"#, qr_svg.replace("width=", "data-width=").replace("height=", "data-height=").replace("<svg", r#"<svg style="width: 100%; height: auto; display: block;""#))))
                }
            }
        }
    }
}

#[derive(Deserialize)]
pub struct CreateInviteForm {
    expiry_days: u64,
    user_limit: u64,
}

// Creates an invite code with the submitted expiration and user limit and
// returns the fragment htmx swaps into the invite card.
pub async fn post_create_invite(
    State(state): State<Arc<ConsensusApi>>,
    Form(form): Form<CreateInviteForm>,
) -> impl IntoResponse {
    let (invite_code, meta) = state.create_invite_code(form.expiry_days, form.user_limit);
    let invite_string = picomint_base32::encode(&invite_code);

    let expiry = DateTime::from_timestamp(meta.expires_at as i64, 0)
        .expect("expiry is a valid timestamp")
        .format("%B %-d, %Y");

    Html(
        html! {
            (qr_code(&invite_string))

            div class="mb-3" {
                (copiable_text(&invite_string))
            }

            div class="alert alert-info" {
                "This invite code expires on " (expiry) " and can be used by up to "
                (meta.user_limit)
                " users."
            }

            button
                class="btn btn-outline-primary w-100 py-2 mt-2"
                hx-post=(INVITE_CREATE_ROUTE)
                hx-target="#invite-container"
                hx-swap="innerHTML"
            {
                "Generate Another Invite Code"
            }
        }
        .into_string(),
    )
}
