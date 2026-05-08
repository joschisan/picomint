use std::sync::Arc;

use axum::extract::{Form, State};
use axum::response::{IntoResponse, Redirect};
use maud::{Markup, html};

use crate::consensus::api::ConsensusApi;
use crate::ui::ROOT_ROUTE;

// LN route constants
pub const LN_ADD_ROUTE: &str = "/ln/add";
pub const LN_REMOVE_ROUTE: &str = "/ln/remove";

// Form for gateway management — `gateway_pk` is the gateway's iroh
// public key, base32-encoded.
#[derive(serde::Deserialize)]
pub struct GatewayForm {
    pub gateway_pk: picomint_core::ln::gateway_api::GatewayPk,
}

// Function to render the Lightning module UI section
pub async fn render(lightning: &crate::consensus::ln::Lightning) -> Markup {
    let gateways = lightning.gateways_ui();
    let consensus_block_count = lightning.consensus_block_count_ui();
    let consensus_unix_time = lightning.consensus_unix_time_ui();
    let wallclock = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let drift = consensus_unix_time as i64 - wallclock as i64;
    let formatted_unix_time = format!("{consensus_unix_time} ({drift:+}s vs wallclock)");

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
                        tr {
                            th { "Consensus Unix Time" }
                            td { (formatted_unix_time) }
                        }
                    }
                }

                // Gateway management
                div {
                    div class="row" {
                        // Left tile - Gateway list or message
                        div class="col-lg-6 pe-lg-4 position-relative" {
                            div class="h-100" {
                                @if gateways.is_empty() {
                                    div class="text-center p-4" {
                                        p { "You need a Lightning gateway to connect to your federation and then add its URL here in the dashboard to enable Lightning payments for your users. You can either run your own gateway or reach out to the Picomint team on " a href="https://chat.picomint.org/" { "Discord" } " - we are running our own gateway and are happy to get you started." }
                                    }
                                } @else {
                                    div class="table-responsive" {
                                        table class="table table-hover" {
                                            tbody {
                                                @for gateway in &gateways {
                                                    tr {
                                                        td { (gateway.to_string()) }
                                                        td class="text-end" {
                                                            form action=(LN_REMOVE_ROUTE) method="post" style="display: inline;" {
                                                                input type="hidden" name="gateway_pk" value=(gateway.to_string());
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
                                }
                            }
                            // Add vertical divider
                            div class="position-absolute end-0 top-0 bottom-0 d-none d-lg-block" style="width: 1px; background-color: #dee2e6;" {}
                        }

                        // Right tile - Add gateway form
                        div class="col-lg-6 ps-lg-4" {
                            div class="d-flex flex-column align-items-center h-100" {
                                form action=(LN_ADD_ROUTE) method="post" class="w-100" style="max-width: 400px;" {
                                    div class="alert alert-warning mb-3" {
                                        "All guardians have to enter the exact same set of gateway node ids for them to be served to clients."
                                    }
                                    div class="mb-3" {
                                        input
                                            type="text"
                                            class="form-control"
                                            id="gateway-node-id"
                                            name="gateway_pk"
                                            placeholder="Enter gateway node id"
                                            required;
                                    }
                                    div class="text-muted mb-3 text-center" style="font-size: 0.875em;" {
                                        "Iroh node id (base32-encoded)"
                                    }
                                    div class="text-center" {
                                        button type="submit" class="btn btn-primary" style="min-width: 150px;" {
                                            "Add Gateway"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// Handler for adding a new gateway
pub async fn post_add(
    State(state): State<Arc<ConsensusApi>>,
    Form(form): Form<GatewayForm>,
) -> impl IntoResponse {
    state.server.ln.add_gateway_ui(form.gateway_pk).await;

    Redirect::to(ROOT_ROUTE).into_response()
}

// Handler for removing a gateway
pub async fn post_remove(
    State(state): State<Arc<ConsensusApi>>,
    Form(form): Form<GatewayForm>,
) -> impl IntoResponse {
    state.server.ln.remove_gateway_ui(form.gateway_pk).await;

    Redirect::to(ROOT_ROUTE).into_response()
}
