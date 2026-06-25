use std::sync::Arc;

use axum::extract::{Form, State};
use axum::response::{Html, IntoResponse};
use maud::{Markup, html};
use picomint_core::ln::gateway::GatewayPk;

use crate::consensus::api::ConsensusApi;

// LN route constants
pub const LN_ADD_ROUTE: &str = "/ln/add";
pub const LN_REMOVE_ROUTE: &str = "/ln/remove";

// Form for gateway management. `gateway_pk` is kept as a raw string so a
// malformed value renders an inline error instead of the extractor
// rejecting the request with a 422.
#[derive(serde::Deserialize)]
pub struct GatewayForm {
    pub gateway_pk: String,
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

// Swappable gateway list + add form. `error`, when set, renders an inline
// alert above the input. Returned both by `render` for the initial page
// and by the add/remove handlers as the htmx fragment.
fn gateway_section(gateways: &[GatewayPk], error: Option<&str>) -> Markup {
    html! {
        // Add gateway form — input and button side by side, full width
        form hx-post=(LN_ADD_ROUTE) hx-target="#gateway-section" hx-swap="innerHTML" {
            div class="alert alert-warning mb-3" {
                "All guardians have to enter the exact same set of gateway node ids for them to be served to clients."
            }
            @if let Some(error) = error {
                div class="alert alert-danger mb-3" { (error) }
            }
            div class="d-flex gap-2" {
                input
                    type="text"
                    class="form-control flex-grow-1"
                    id="gateway-node-id"
                    name="gateway_pk"
                    placeholder="Enter Gateway Code"
                    required;
                button type="submit" class="btn btn-primary" style="min-width: 150px;" {
                    "Add Gateway"
                }
            }
        }

        // Gateway list below the form, or empty-state message. Each code
        // truncates with an ellipsis (`text-truncate` + `min-width: 0` on
        // the flex child) so the Remove button is never pushed off a
        // narrow viewport.
        @if gateways.is_empty() {
            div class="text-center p-4" {
                p { "You need a Lightning gateway to connect to your federation and then add its URL here in the dashboard to enable Lightning payments for your users. You can either run your own gateway or reach out to the Picomint team on " a href="https://chat.picomint.org/" { "Discord" } " - we are running our own gateway and are happy to get you started." }
            }
        } @else {
            div class="list-group mt-4" {
                @for gateway in gateways {
                    @let encoded = picomint_base32::encode(gateway);
                    div class="list-group-item d-flex align-items-center gap-2" {
                        span class="text-truncate flex-grow-1" style="min-width: 0;" {
                            (encoded)
                        }
                        form hx-post=(LN_REMOVE_ROUTE) hx-target="#gateway-section" hx-swap="innerHTML" class="flex-shrink-0" {
                            input type="hidden" name="gateway_pk" value=(encoded);
                            button type="submit" class="btn btn-sm btn-danger" {
                                "Remove"
                            }
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
    Form(form): Form<GatewayForm>,
) -> impl IntoResponse {
    let Ok(gateway_pk) = form.gateway_pk.trim().parse::<GatewayPk>() else {
        let gateways = state.server.ln.gateways_ui();

        return Html(gateway_section(&gateways, Some("Invalid gateway code")).into_string());
    };

    state.server.ln.add_gateway_ui(gateway_pk).await;

    let gateways = state.server.ln.gateways_ui();

    Html(gateway_section(&gateways, None).into_string())
}

// Handler for removing a gateway. The submitted value is the already-valid
// encoded key from the list, so a parse failure is a no-op.
pub async fn post_remove(
    State(state): State<Arc<ConsensusApi>>,
    Form(form): Form<GatewayForm>,
) -> impl IntoResponse {
    if let Ok(gateway_pk) = form.gateway_pk.trim().parse::<GatewayPk>() {
        state.server.ln.remove_gateway_ui(gateway_pk).await;
    }

    let gateways = state.server.ln.gateways_ui();

    Html(gateway_section(&gateways, None).into_string())
}
