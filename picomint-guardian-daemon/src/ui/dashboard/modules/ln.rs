use std::sync::Arc;

use axum::extract::{Form, State};
use axum::response::{Html, IntoResponse};
use maud::{Markup, html};
use picomint_core::ln::gateway::GatewayPk;

use crate::consensus::api::ConsensusApi;

// LN route constants
pub const LN_ADD_ROUTE: &str = "/ln/add";
pub const LN_REMOVE_ROUTE: &str = "/ln/remove";

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

// Function to render the Lightning module UI section
pub async fn render(lightning: &crate::consensus::ln::Lightning) -> Markup {
    let gateways = lightning.gateways_ui();
    let consensus_block_count = lightning.consensus_block_count_ui();

    html! {
        div class="card h-100" {
            div class="card-header dashboard-header" { "Lightning" }
            div class="card-body" {
                // Consensus status information
                div class="mb-4" {
                    table
                        class="table"
                        id="ln-module-timers" hx-swap-oob=(true)
                    {
                        tr {
                            th { "Consensus Block Count" }
                            td { (consensus_block_count) }
                        }
                    }
                }

                // Gateway management — htmx swaps this section in place on
                // add/remove and on a validation error, so no full reload.
                div id="gateway-section" {
                    (gateway_section(&gateways, None))
                }
            }
        }
    }
}

// Swappable gateway management split: list of named gateways on the left,
// add form on the right, divided on md+ screens. `error`, when set, renders
// an inline alert above the form inputs. Returned both by `render` for the
// initial page and by the add/remove handlers as the htmx fragment.
fn gateway_section(gateways: &[(GatewayPk, String)], error: Option<&str>) -> Markup {
    html! {
        div class="row g-4" {
            div class="col-md" {
                @if gateways.is_empty() {
                    div class="text-center p-4" {
                        p { "You need a Lightning gateway to connect to your federation and then add it here in the dashboard to enable Lightning payments for your users." }
                    }
                } @else {
                    div class="list-group" {
                        @for (pk, name) in gateways {
                            div class="list-group-item d-flex align-items-center gap-2" {
                                span class="text-truncate flex-grow-1" style="min-width: 0;" {
                                    (name)
                                }
                                form hx-post=(LN_REMOVE_ROUTE) hx-target="#gateway-section" hx-swap="innerHTML" class="flex-shrink-0" {
                                    input type="hidden" name="pk" value=(picomint_base32::encode(pk));
                                    button type="submit" class="btn btn-sm btn-danger" {
                                        "Remove"
                                    }
                                }
                            }
                        }
                    }
                }
            }
            div class="col-md-auto d-none d-md-block p-0" {
                div class="vr h-100" {}
            }
            div class="col-md" {
                form hx-post=(LN_ADD_ROUTE) hx-target="#gateway-section" hx-swap="innerHTML" {
                    div class="alert alert-warning mb-3" {
                        "All guardians have to enter the exact same set of gateways."
                    }
                    @if let Some(error) = error {
                        div class="alert alert-danger mb-3" { (error) }
                    }
                    div class="mb-3" {
                        input
                            type="text"
                            class="form-control"
                            id="gateway-node-id"
                            name="pk"
                            placeholder="Enter Gateway Code"
                            required;
                    }
                    div class="mb-3" {
                        input
                            type="text"
                            class="form-control"
                            id="gateway-name"
                            name="name"
                            placeholder="Enter Nickname"
                            required;
                    }
                    div class="d-grid" {
                        button type="submit" class="btn btn-primary" {
                            "Add Gateway"
                        }
                    }
                }
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
    let gateways = state.server.ln.gateways_ui();

    let Ok(pk) = form.pk.trim().parse::<GatewayPk>() else {
        return Html(gateway_section(&gateways, Some("Invalid gateway code")).into_string());
    };

    let name = form.name.trim().to_string();

    if name.is_empty() {
        return Html(gateway_section(&gateways, Some("Name must not be empty")).into_string());
    }

    state.server.ln.add_gateway_ui(pk, name).await;

    let gateways = state.server.ln.gateways_ui();

    Html(gateway_section(&gateways, None).into_string())
}

// Handler for removing a gateway. The submitted value is the already-valid
// encoded key from the list, so a parse failure is a no-op.
pub async fn post_remove(
    State(state): State<Arc<ConsensusApi>>,
    Form(form): Form<RemoveGatewayForm>,
) -> impl IntoResponse {
    if let Ok(pk) = form.pk.trim().parse::<GatewayPk>() {
        state.server.ln.remove_gateway_ui(pk).await;
    }

    let gateways = state.server.ln.gateways_ui();

    Html(gateway_section(&gateways, None).into_string())
}
