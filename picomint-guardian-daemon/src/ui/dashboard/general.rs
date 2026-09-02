use std::collections::BTreeMap;

use maud::{Markup, html};
use picomint_core::PeerId;

use crate::p2p::{P2PConnectionStatus, Transport};

/// Renders the federation card: one row per peer with its name and p2p
/// connection status, with the federation's consensus progress below. The
/// running guardian has no connection to itself and is omitted.
pub fn render(
    federation_name: &str,
    guardian_names: &BTreeMap<PeerId, String>,
    p2p_connection_status: &BTreeMap<PeerId, P2PConnectionStatus>,
    session_count: u64,
    block_count: u64,
) -> Markup {
    html! {
        div class="card h-100" {
            div class="card-header dashboard-header" { (federation_name) }
            div class="card-body" {
                table class="table table-striped mb-4" {
                    thead {
                        tr {
                            th { "ID" }
                            th { "Name" }
                            th { "Status" }
                            th { "Transport" }
                            th { "Round Trip" }
                        }
                    }
                    tbody {
                        @for (peer, status) in p2p_connection_status {
                            tr {
                                td { (peer.to_string()) }
                                td { (guardian_names.get(peer).expect("every peer is in the consensus config")) }
                                (connection_cells(status))
                            }
                        }
                    }
                }

                table class="table mb-0" {
                    tr {
                        th { "Session Count" }
                        td { (session_count) }
                    }
                    tr {
                        th { "Block Count" }
                        td { (block_count) }
                    }
                }
            }
        }
    }
}

fn connection_cells(status: &P2PConnectionStatus) -> Markup {
    let path = match status {
        P2PConnectionStatus::Connected(path) => Some(path),
        P2PConnectionStatus::Disconnected => None,
    };

    html! {
        td {
            @match path {
                Some(_) => {
                    span class="badge bg-success" { "Connected" }
                }
                None => {
                    span class="badge bg-danger" { "Disconnected" }
                }
            }
        }
        td {
            @match path {
                Some(path) => {
                    @match path.transport {
                        Transport::Direct => {
                            span class="badge bg-success" title=(path.remote_addr) { "Direct" }
                        }
                        Transport::Relay => {
                            span class="badge bg-warning text-dark" title=(path.remote_addr) { "Relay" }
                        }
                    }
                }
                None => {
                    span class="text-muted" { "—" }
                }
            }
        }
        td {
            @match path {
                Some(path) => {
                    (format!("{} ms", path.rtt.as_millis()))
                }
                None => {
                    span class="text-muted" { "N/A" }
                }
            }
        }
    }
}
