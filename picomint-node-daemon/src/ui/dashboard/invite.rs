use std::sync::Arc;

use axum::extract::{Form, State};
use axum::response::{Html, IntoResponse};
use chrono::DateTime;
use maud::{Markup, PreEscaped, html};
use picomint_node_cli_core::{
    DEFAULT_INVITE_EXPIRY_DAYS, DEFAULT_INVITE_USER_LIMIT, INVITE_EXPIRY_DAYS_LIMIT,
};
use qrcode::QrCode;
use serde::Deserialize;

use crate::consensus::api::ConsensusApi;
use crate::ui::{copiable_text, modal_header};

pub const INVITE_CREATE_ROUTE: &str = "/invite/create";

// Invite dialog, opened from the Actions launcher. It starts on the
// generation form; the generated code replaces the dialog content via
// htmx.
pub fn render(consensus_block_count: u32) -> Markup {
    html! {
        dialog id="invite-modal" autofocus {
            (modal_header("Generate Invite"))
            @if consensus_block_count == 0 {
                div class="modal-body" {
                    div class="alert alert-warning" {
                        "Invite codes will be available once the mint has reached consensus on a block count."
                    }
                }
            } @else {
                div id="invite-container" {
                    (generate_form(None))
                }
            }
        }
    }
}

// Form that asks the node for a fresh invite code with an operator-chosen
// expiration (in days) and user limit, pre-filled with the defaults. The
// generated code replaces the form via htmx; `error`, when set, renders an
// inline alert above the inputs.
fn generate_form(error: Option<&str>) -> Markup {
    html! {
        div class="modal-body" {
            form class="form-stack" hx-post=(INVITE_CREATE_ROUTE) hx-target="#invite-container" hx-swap="innerHTML" {
                @if let Some(error) = error {
                    div class="alert alert-danger" { (error) }
                }

                div class="field" {
                    label class="field-label" for="expiry_days" { "Expiration (days)" }
                    input
                        type="number"
                        id="expiry_days"
                        name="expiry_days"
                        min="1"
                        max=(INVITE_EXPIRY_DAYS_LIMIT)
                        value=(DEFAULT_INVITE_EXPIRY_DAYS)
                        required;
                }

                div class="field" {
                    label class="field-label" for="user_limit" { "Maximum users" }
                    input
                        type="number"
                        id="user_limit"
                        name="user_limit"
                        min="1"
                        value=(DEFAULT_INVITE_USER_LIMIT)
                        required;
                }

                button type="submit" class="btn btn-primary btn-lg btn-block" { "Generate Invite" }
            }
        }
    }
}

fn qr_code(data: &str) -> Markup {
    let qr_svg = QrCode::new(data)
        .expect("invite codes are far below the QR data capacity")
        .render::<qrcode::render::svg::Color>()
        .build();

    html! {
        div class="qr-box" {
            (PreEscaped(qr_svg))
        }
    }
}

#[derive(Deserialize)]
pub struct CreateInviteForm {
    expiry_days: u64,
    user_limit: u32,
}

// Creates an invite code with the submitted expiration and user limit and
// returns the fragment htmx swaps into the invite modal.
pub async fn post_create_invite(
    State(state): State<Arc<ConsensusApi>>,
    Form(form): Form<CreateInviteForm>,
) -> impl IntoResponse {
    if form.expiry_days > INVITE_EXPIRY_DAYS_LIMIT {
        return Html(
            generate_form(Some(&format!(
                "Expiration must be at most {INVITE_EXPIRY_DAYS_LIMIT} days"
            )))
            .into_string(),
        );
    }

    let (invite_code, meta) = state.create_invite_code(form.expiry_days, form.user_limit);
    let invite_string = picomint_base32::encode(&invite_code);

    let expiry = DateTime::from_timestamp(meta.expires_at as i64, 0)
        .expect("expiry is a valid timestamp")
        .format("%B %-d, %Y");

    Html(
        html! {
            div class="modal-body" {
                (qr_code(&invite_string))

                (copiable_text(&invite_string))

                div class="alert alert-info" {
                    "This invite code expires on " (expiry) " and can be used by up to "
                    (meta.user_limit)
                    " users."
                }
            }
        }
        .into_string(),
    )
}
