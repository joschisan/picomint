use std::sync::Arc;

use axum::extract::{Form, State};
use axum::response::{Html, IntoResponse};
use chrono::{Datelike, Months, Utc};
use maud::{Markup, html};
use picomint_core::expiry::ExpiryStatus;
use picomint_core::invite::InviteCode;
use serde::Deserialize;

use crate::consensus::api::ConsensusApi;
use crate::ui::dashboard::{CLEAR_EXPIRY_ROUTE, SET_EXPIRY_ROUTE};

/// Form payload for [`post_set`]. Both fields are kept as raw strings so a
/// malformed value renders an inline error instead of the extractor
/// rejecting the request with a 422.
#[derive(Debug, Deserialize)]
pub struct ExpiryForm {
    pub expiry_timestamp: String,
    pub successor_invite_code: Option<String>,
}

pub fn render(status: Option<&ExpiryStatus>) -> Markup {
    html! {
        div class="card h-100" {
            div class="card-header dashboard-header" { "Federation Expiry" }
            div class="card-body" {
                div id="expiry-section" {
                    (expiry_section(status, None))
                }
            }
        }
    }
}

// Swappable expiry management: the announced status with a clear button, or
// the announce form. `error`, when set, renders an inline alert above the
// form inputs. Returned both by `render` for the initial page and by the
// set/clear handlers as the htmx fragment, so no full reload.
fn expiry_section(status: Option<&ExpiryStatus>, error: Option<&str>) -> Markup {
    html! {
        @if let Some(status) = status {
            div class="alert alert-info" {
                strong { "Expiry Announced" }
                @if let Some(date) = chrono::DateTime::from_timestamp(status.timestamp as i64, 0) {
                    strong { " - " (date.format("%B %-d, %Y")) }
                }
                @if let Some(ref successor) = status.successor {
                    p class="mb-0 mt-2 font-monospace text-break" {
                        (picomint_base32::encode(successor))
                    }
                }
            }
            form hx-post=(CLEAR_EXPIRY_ROUTE) hx-target="#expiry-section" hx-swap="innerHTML" {
                button type="submit" class="btn btn-primary w-100" {
                    "Clear Expiry Announcement"
                }
            }
        } @else {
            div class="alert alert-warning" {
                "All guardians have to enter the exact same values."
            }
            @if let Some(error) = error {
                div class="alert alert-danger" { (error) }
            }
            form hx-post=(SET_EXPIRY_ROUTE) hx-target="#expiry-section" hx-swap="innerHTML" {
                div class="mb-3" {
                    select class="form-select" id="expiry_timestamp" name="expiry_timestamp" required {
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
                }
                div class="mb-3" {
                    input
                        type="text"
                        class="form-control"
                        id="successor_invite_code"
                        name="successor_invite_code"
                        placeholder="Enter Optional Invite Code";
                }
                button type="submit" class="btn btn-primary w-100" {
                    "Announce Expiry"
                }
            }
        }
    }
}

// Parses the submitted expiry values and, on failure, re-renders the
// section with an inline error.
pub async fn post_set(
    State(state): State<Arc<ConsensusApi>>,
    Form(form): Form<ExpiryForm>,
) -> impl IntoResponse {
    let Ok(timestamp) = form.expiry_timestamp.parse::<u64>() else {
        return Html(expiry_section(None, Some("Invalid expiry date")).into_string());
    };

    let invite_input = form.successor_invite_code.filter(|s| !s.trim().is_empty());

    let successor = match &invite_input {
        Some(s) => match picomint_base32::decode::<InviteCode>(s.trim()) {
            Ok(code) => Some(code),
            Err(_) => {
                return Html(
                    expiry_section(None, Some("Invalid invite code format")).into_string(),
                );
            }
        },
        None => None,
    };

    let status = ExpiryStatus {
        timestamp,
        successor,
    };

    state.set_expiry_status(Some(status.clone()));

    Html(expiry_section(Some(&status), None).into_string())
}

pub async fn post_clear(State(state): State<Arc<ConsensusApi>>) -> impl IntoResponse {
    state.set_expiry_status(None);

    Html(expiry_section(None, None).into_string())
}
