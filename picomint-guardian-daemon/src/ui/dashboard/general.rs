use std::collections::BTreeMap;

use maud::{Markup, html};
use picomint_core::NodeId;

use crate::p2p::{P2PConnectionStatus, Transport};

/// Renders the mint card: one row per node with its name and p2p
/// connection status. The running guardian has no connection to itself and
/// is omitted.
pub fn render(
    mint_name: &str,
    guardian_names: &BTreeMap<NodeId, String>,
    p2p_connection_status: &BTreeMap<NodeId, P2PConnectionStatus>,
) -> Markup {
    let connected = p2p_connection_status
        .values()
        .filter(|status| matches!(status, P2PConnectionStatus::Connected(..)))
        .count();

    html! {
        div class="card" {
            div class="card-header" {
                span class="card-title" { (mint_name) }
                span class="card-sub" {
                    (format!("{connected} of {} nodes connected", p2p_connection_status.len()))
                }
            }
            table {
                thead {
                    tr {
                        th { "ID" }
                        th { "Name" }
                        th { "Status" }
                        th { "Transport" }
                        th style="text-align: right" { "Round Trip" }
                    }
                }
                tbody {
                    @for (node, status) in p2p_connection_status {
                        tr {
                            td class="mono muted" { (node.to_string()) }
                            td { (guardian_names.get(node).expect("every node is in the consensus config")) }
                            (connection_cells(status))
                        }
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
                    span class="status status-ok" { "Connected" }
                }
                None => {
                    span class="status status-bad" { "Disconnected" }
                }
            }
        }
        td {
            @match path {
                Some(path) => {
                    @match path.transport {
                        Transport::Direct => {
                            span class="badge badge-direct" title=(path.remote_addr) { "Direct" }
                        }
                        Transport::Relay => {
                            span class="badge badge-relay" title=(path.remote_addr) { "Relay" }
                        }
                    }
                }
                None => {
                    span class="muted" { "—" }
                }
            }
        }
        td class="mono" style="text-align: right" {
            @match path {
                Some(path) => {
                    (format!("{} ms", path.rtt.as_millis()))
                }
                None => {
                    span class="muted" { "N/A" }
                }
            }
        }
    }
}
