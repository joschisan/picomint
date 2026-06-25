use std::collections::BTreeMap;

use crate::p2p::{P2PConnectionStatus, Transport};
use maud::{Markup, html};
use picomint_core::PeerId;

pub fn render(p2p_connection_status: &BTreeMap<PeerId, P2PConnectionStatus>) -> Markup {
    html! {
        div class="card h-100" id="peer-connections" {
            div class="card-header dashboard-header" { "Peer Connections" }
            div class="card-body" {
                @if p2p_connection_status.is_empty() {
                    p { "No peer connections available." }
                } @else {
                    table class="table table-striped" {
                        thead {
                            tr {
                                th { "ID" }
                                th { "Status" }
                                th { "Transport" }
                                th { "Round Trip" }
                            }
                        }
                        tbody {
                            @for (peer, status) in p2p_connection_status {
                                @let path = match status {
                                    P2PConnectionStatus::Connected(path) => Some(path),
                                    P2PConnectionStatus::Disconnected => None,
                                };
                                tr {
                                    td { (peer.to_string()) }
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
                        }
                    }
                }
            }
        }
    }
}
