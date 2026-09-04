use std::sync::Arc;

use axum::extract::{Form, State};
use axum::response::{Html, IntoResponse};
use chrono::{Datelike, Months, Utc};
use maud::{Markup, html};
use picomint_core::expiry::ExpiryStatus;
use picomint_core::invite::InviteCode;
use serde::Deserialize;

use crate::consensus::api::ConsensusApi;
use crate::ui::dashboard::SET_EXPIRY_ROUTE;

/// Form payload for [`post_set`]. Both fields are kept as raw strings so a
/// malformed value renders an inline error instead of the extractor
/// rejecting the request with a 422.
#[derive(Debug, Deserialize)]
pub struct ExpiryForm {
    pub expiry_timestamp: String,
    pub successor_invite_code: Option<String>,
}

// The announce form inside the expiry modal. `error`, when set, renders an
// inline alert above the inputs; the swap target sits inside the open
// dialog, so it stays visible. A successful announce refreshes the whole
// page instead, which closes the modal and flips the launcher item to
// "Remove Expiry Date".
pub fn expiry_form(error: Option<&str>) -> Markup {
    html! {
        form class="form-stack" hx-post=(SET_EXPIRY_ROUTE) hx-target="#expiry-section" hx-swap="innerHTML" {
            div class="alert alert-warning" {
                "All nodes have to enter the exact same values."
            }
            @if let Some(error) = error {
                div class="alert alert-danger" { (error) }
            }
            select id="expiry_timestamp" name="expiry_timestamp" required {
                option value="" selected disabled { "Select Expiry Date" }
                @let now = Utc::now();
                @for i in 1..=12u32 {
                    @let last_day = now.date_naive()
                        .with_day(1).expect("day 1 is always valid")
                        .checked_add_months(Months::new(i + 1))
                        .expect("adding months to current date can't overflow")
                        .pred_opt()
                        .expect("predecessor of first of month is always valid");
                    @let timestamp = last_day
                        .and_hms_opt(0, 0, 0).expect("midnight is always valid")
                        .and_utc()
                        .timestamp();
                    option value=(timestamp) {
                        (last_day.format("%B %-d, %Y"))
                    }
                }
            }
            input
                type="text"
                id="successor_invite_code"
                name="successor_invite_code"
                placeholder="Enter Optional Invite Code";
            button type="submit" class="btn btn-primary btn-lg btn-block" { "Announce Expiry" }
        }
    }
}

// Parses the submitted expiry values; on failure re-renders the form with
// an inline error, on success refreshes the page.
pub async fn post_set(
    State(state): State<Arc<ConsensusApi>>,
    Form(form): Form<ExpiryForm>,
) -> impl IntoResponse {
    let Ok(timestamp) = form.expiry_timestamp.parse::<u64>() else {
        return Html(expiry_form(Some("Invalid expiry date")).into_string()).into_response();
    };

    let invite_input = form.successor_invite_code.filter(|s| !s.trim().is_empty());

    let successor = match &invite_input {
        Some(s) => match picomint_base32::decode::<InviteCode>(s.trim()) {
            Ok(code) => Some(code),
            Err(_) => {
                return Html(expiry_form(Some("Invalid invite code format")).into_string())
                    .into_response();
            }
        },
        None => None,
    };

    state.set_expiry_status(Some(ExpiryStatus {
        timestamp,
        successor,
    }));

    ([("HX-Refresh", "true")], Html(String::new())).into_response()
}

// Clears the announcement, triggered straight from the launcher item, and
// refreshes the page so the item flips back to "Announce Expiry Date".
pub async fn post_clear(State(state): State<Arc<ConsensusApi>>) -> impl IntoResponse {
    state.set_expiry_status(None);

    ([("HX-Refresh", "true")], Html(String::new()))
}
