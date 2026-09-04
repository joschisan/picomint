use std::sync::Arc;

use axum::extract::{Form, State};
use axum::response::{Html, IntoResponse};
use maud::{Markup, PreEscaped, html};
use picomint_core::lightning::gateway::GatewayPk;
use picomint_redb::ReadTx;

use crate::consensus::api::ConsensusApi;
use crate::consensus::lightning;
use crate::ui::modal_header;

pub const LN_ADD_ROUTE: &str = "/lightning/add";
pub const LN_REMOVE_ROUTE: &str = "/lightning/remove";

// Form for adding a gateway. `pk` is kept as a raw string so a
// malformed value renders an inline error instead of the extractor
// rejecting the request with a 422.
#[derive(serde::Deserialize)]
pub struct AddGatewayForm {
    pub pk: String,
    pub name: String,
}

#[derive(serde::Deserialize)]
pub struct RemoveGatewayForm {
    pub pk: String,
}

// Gateway management — htmx swaps this section in place on add/remove and
// on a validation error, so no full reload. The add form lives in a
// <dialog> the Actions launcher opens; a swap re-renders the dialog
// closed, and the error fragment reopens it so the inline error is
// visible. The section wraps the card rather than the other way around,
// so the card disappears with the last gateway while the dialog stays
// reachable.
pub fn render(dbtx: &ReadTx) -> Markup {
    let gateways = lightning::gateways(dbtx);

    html! {
        div id="gateway-section" {
            (gateway_section(&gateways, None))
        }
    }
}

// Swappable gateway management: the card listing named gateways (absent
// when there are none) plus the add-gateway modal. `error`, when set,
// renders an inline alert above the form inputs and reopens the modal
// after the swap. Returned both by `render` for the initial page and by
// the add/remove handlers as the htmx fragment.
fn gateway_section(gateways: &[(GatewayPk, String)], error: Option<&str>) -> Markup {
    html! {
        @if !gateways.is_empty() {
            div class="card" {
                div class="card-header" {
                    span class="card-title" { "Lightning Gateways" }
                }
                div class="list" {
                    @for (pk, name) in gateways {
                        @let encoded = picomint_base32::encode(pk);
                        div class="list-row" {
                            span class="list-row-name" { (name) }
                            span class="mono muted" style="font-size: 12px" title=(encoded) {
                                (encoded[..6]) "…" (encoded[encoded.len() - 4..])
                            }
                            form hx-post=(LN_REMOVE_ROUTE) hx-target="#gateway-section" hx-swap="innerHTML" {
                                input type="hidden" name="pk" value=(encoded);
                                button type="submit" class="link-danger" { "Remove" }
                            }
                        }
                    }
                }
            }
        }

        dialog id="gateway-modal" autofocus {
            (modal_header("Add Gateway"))
            div class="modal-body" {
                form class="form-stack" hx-post=(LN_ADD_ROUTE) hx-target="#gateway-section" hx-swap="innerHTML" {
                    div class="alert alert-warning" {
                        "All guardians have to enter the exact same set of gateways."
                    }
                    @if let Some(error) = error {
                        div class="alert alert-danger" { (error) }
                    }
                    input type="text" name="pk" placeholder="Enter Gateway Code" required;
                    input type="text" name="name" placeholder="Enter Nickname" required;
                    button type="submit" class="btn btn-primary btn-lg btn-block" { "Add Gateway" }
                }
            }
        }

        @if error.is_some() {
            script {
                (PreEscaped("document.getElementById('gateway-modal').showModal()"))
            }
        }
    }
}

// Handler for adding a new gateway. Parses the submitted code and, on
// failure, re-renders the section with an inline error.
pub async fn post_add(
    State(state): State<Arc<ConsensusApi>>,
    Form(form): Form<AddGatewayForm>,
) -> impl IntoResponse {
    let gateways = lightning::gateways(&state.server.db.begin_read());

    let Ok(pk) = form.pk.trim().parse::<GatewayPk>() else {
        return Html(gateway_section(&gateways, Some("Invalid gateway code")).into_string());
    };

    let name = form.name.trim().to_string();

    if name.is_empty() {
        return Html(gateway_section(&gateways, Some("Name must not be empty")).into_string());
    }

    lightning::add_gateway(&state.server, pk, name);

    let gateways = lightning::gateways(&state.server.db.begin_read());

    Html(gateway_section(&gateways, None).into_string())
}

// Handler for removing a gateway. The submitted value is the already-valid
// encoded key from the list, so a parse failure is a no-op.
pub async fn post_remove(
    State(state): State<Arc<ConsensusApi>>,
    Form(form): Form<RemoveGatewayForm>,
) -> impl IntoResponse {
    if let Ok(pk) = form.pk.trim().parse::<GatewayPk>() {
        lightning::remove_gateway(&state.server, pk);
    }

    let gateways = lightning::gateways(&state.server.db.begin_read());

    Html(gateway_section(&gateways, None).into_string())
}
