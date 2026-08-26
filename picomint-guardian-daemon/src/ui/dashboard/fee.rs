use std::sync::Arc;

use axum::extract::{Form, State};
use axum::response::{Html, IntoResponse};
use maud::{Markup, html};
use picomint_core::fee::FeeConfig;
use serde::Deserialize;

use crate::consensus::api::ConsensusApi;
use crate::ui::dashboard::{CLEAR_FEE_ROUTE, SET_FEE_ROUTE};

/// The fees a guardian may announce from here, in parts per million —
/// a hundredth of a percent up to a tenth of one.
///
/// A list rather than a number field, and a bound on what the dashboard
/// offers rather than on what consensus will honour. Every guardian has to
/// enter the identical value for an announcement to take effect at all, and
/// agreeing on a value picked from a list is a different problem from
/// agreeing on one typed into a box where two extra zeroes are a
/// hundredfold.
const PPM_OPTIONS: [u64; 10] = [100, 200, 300, 400, 500, 600, 700, 800, 900, 1000];

/// Form payload for [`post_set`]. The ppm arrives as a string and is
/// validated in the handler rather than by the form, so a bad entry comes
/// back as an inline message instead of the browser's own.
#[derive(Debug, Deserialize)]
pub struct FeeForm {
    pub ppm: String,
    pub lnurl: String,
}

pub fn render(fee: Option<&FeeConfig>) -> Markup {
    html! {
        div class="card h-100" {
            div class="card-header dashboard-header" { "Federation Fee" }
            div class="card-body" {
                // htmx swaps this section in place on announce/clear and on
                // a validation error, so no full reload.
                div id="fee-section" {
                    (fee_section(fee, None))
                }
            }
        }
    }
}

/// Swappable announcement view + form. `error`, when set, renders an inline
/// alert above the input. Returned both by [`render`] for the initial page
/// and by the handlers as the htmx fragment.
fn fee_section(fee: Option<&FeeConfig>, error: Option<&str>) -> Markup {
    html! {
        @if let Some(fee) = fee {
            div class="alert alert-info" {
                strong { "Fee Announced" }
                strong { " - " (percent(fee.ppm)) }
                p class="mb-0 mt-2 font-monospace text-break" { (fee.lnurl) }
            }
            form hx-post=(CLEAR_FEE_ROUTE) hx-target="#fee-section" hx-swap="innerHTML" {
                button type="submit" class="btn btn-primary w-100" {
                    "Clear Fee Announcement"
                }
            }
        } @else {
            form hx-post=(SET_FEE_ROUTE) hx-target="#fee-section" hx-swap="innerHTML" {
                div class="alert alert-warning" {
                    "All guardians have to enter the exact same values."
                }
                @if let Some(error) = error {
                    div class="alert alert-danger" { (error) }
                }
                div class="form-group mb-3" {
                    select class="form-select" id="ppm" name="ppm" required {
                        option value="" selected disabled { "Select Fee" }
                        @for ppm in PPM_OPTIONS {
                            option value=(ppm) { (percent(ppm)) }
                        }
                    }
                }
                div class="form-group mb-3" {
                    input
                        type="text"
                        class="form-control"
                        id="lnurl"
                        name="lnurl"
                        placeholder="Enter Payout Lnurl"
                        required;
                }
                button type="submit" class="btn btn-primary w-100" {
                    "Announce Fee"
                }
            }
        }
    }
}

// Handler for announcing a fee. Parses the submitted ppm and, on failure,
// re-renders the section with an inline error.
pub async fn post_set(
    State(state): State<Arc<ConsensusApi>>,
    Form(form): Form<FeeForm>,
) -> impl IntoResponse {
    // The values are ours, so anything else came from something other than
    // the form we rendered — checked rather than trusted, since the select's
    // options travel no further than the browser.
    let ppm = match form.ppm.trim().parse::<u64>() {
        Ok(ppm) if PPM_OPTIONS.contains(&ppm) => ppm,
        _ => return Html(fee_section(None, Some("Fee is not one on offer")).into_string()),
    };

    // Not checked for being a well-formed lnurl: that needs the bech32
    // decoder, which lives behind a crate the guardian would otherwise have
    // no reason to carry. A guardian who mistypes one fails closed anyway —
    // the announcement only takes effect when every guardian's bytes match,
    // so one wrong entry means no fee is charged at all rather than a fee
    // accruing somewhere it can never be swept from.
    let fee = FeeConfig {
        ppm,
        lnurl: form.lnurl.trim().to_string(),
    };

    state.set_fee_config_ui(Some(fee));

    Html(fee_section(state.fee_config_ui().as_ref(), None).into_string())
}

pub async fn post_clear(State(state): State<Arc<ConsensusApi>>) -> impl IntoResponse {
    state.set_fee_config_ui(None);

    Html(fee_section(None, None).into_string())
}

/// A ppm as the percentage a guardian picked it as, so the tile reads back
/// in the units the dropdown offered rather than the ones it stores.
fn percent(ppm: u64) -> String {
    format!("{:.2}%", ppm as f64 / 10_000.0)
}
