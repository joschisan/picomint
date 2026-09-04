//! Guardian admin web UI.
//!
//! The UI runs in three phases on the same TCP port:
//!
//! - Setup UI (before the operator confirms the peer set). Served by
//!   [`setup::router`] which takes an `Arc<SetupApi>` directly.
//! - DKG UI (after the operator clicks "Start DKG" until consensus comes
//!   up). Served by [`dkg::router`]: a stateless fallback router that
//!   returns the same waiting page for every path with status 503. The
//!   page polls `/` and redirects once the consensus UI starts answering
//!   with status 200.
//! - Dashboard UI (once the federation is running). Served by
//!   [`dashboard::router`] which takes an `Arc<ConsensusApi>` and reaches
//!   straight into the three typed module instances (`ecash`, `wallet`, `ln`)
//!   hanging off it.
//!
//! The UI is unauthenticated. Operators are expected to bind it to loopback
//! (or expose it via SSH tunnel / VPN). See README.md for the deployment
//! patterns.
//!
//! Styling is a single hand-rolled stylesheet (`assets/style.css`); modals
//! are native `<dialog>` elements opened and closed with one-line inline
//! handlers, so htmx is the only vendored JS.

pub mod assets;
pub mod dashboard;
pub mod dkg;
pub mod setup;

use std::net::SocketAddr;

use axum::Router;
use maud::{DOCTYPE, Markup, PreEscaped, html};
use tokio::net::TcpListener;
use tracing::info;

pub const ROOT_ROUTE: &str = "/";

/// Phase UI server — binds `ui_addr` and serves `router` until the caller
/// aborts the task, which drops the listener and releases the port for the
/// next phase to rebind.
pub async fn run(ui_addr: SocketAddr, router: Router) {
    info!("Running UI at http://{} 🚀", ui_addr);

    let listener = TcpListener::bind(ui_addr).await.expect("Failed to bind UI");

    axum::serve(listener, router.into_make_service())
        .await
        .expect("Failed to serve UI");
}

pub fn common_head(title: &str) -> Markup {
    html! {
        meta charset="utf-8";
        meta name="viewport" content="width=device-width, initial-scale=1.0";
        link rel="stylesheet" type="text/css" href=(*assets::STYLE_CSS_HREF);

        // Note: this needs to be included in the header, so that web-page does not
        // get in a state where htmx is not yet loaded. `defer` helps with blocking the load.
        // Learned the hard way. --dpc
        script defer src="/assets/htmx.org-2.0.4.min.js" {}

        title { (title) }

        script {
            (PreEscaped(r#"
            function copyText(text, btn) {
                if (navigator.clipboard) {
                    navigator.clipboard.writeText(text).then(function() {
                        showCopied(btn);
                    });
                } else {
                    var ta = document.createElement('textarea');
                    ta.value = text;
                    ta.style.position = 'fixed';
                    ta.style.opacity = '0';
                    document.body.appendChild(ta);
                    ta.select();
                    document.execCommand('copy');
                    document.body.removeChild(ta);
                    showCopied(btn);
                }
            }
            function showCopied(btn) {
                if (!btn) return;
                btn.classList.add('copied');
                var icon = btn.innerHTML;
                btn.innerHTML = '&#10003;';
                setTimeout(function() {
                    btn.innerHTML = icon;
                    btn.classList.remove('copied');
                }, 2000);
            }
            "#))
        }
    }
}

pub fn single_card_layout(header: &str, content: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html {
            head {
                (common_head("Picomint"))
            }
            body {
                div class="center-page" {
                    div class="card center-card" {
                        div class="card-header" {
                            span class="card-title" { (header) }
                        }
                        div class="card-body" {
                            (content)
                        }
                    }
                }
            }
        }
    }
}

fn clipboard_icon() -> Markup {
    html! {
        svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" {
            rect x="5.5" y="5.5" width="9" height="9" rx="1.5" {}
            path d="M10.5 5.5 V3 a1.5 1.5 0 0 0 -1.5 -1.5 H3 A1.5 1.5 0 0 0 1.5 3 v6 A1.5 1.5 0 0 0 3 10.5 h2.5" {}
        }
    }
}

/// Renders a readonly text snippet with a copy-to-clipboard button.
pub fn copiable_text(text: &str) -> Markup {
    html! {
        div class="copy-group" {
            span class="copy-text" { (text) }
            button type="button" class="btn btn-outline btn-icon"
                onclick=(format!("copyText('{}', this)", text)) {
                (clipboard_icon())
            }
        }
    }
}

/// Renders a chevron-down glyph used on collapsed disclosure rows.
pub fn chevron_icon() -> Markup {
    html! {
        svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" {
            path d="M4 6 L8 10 L12 6" {}
        }
    }
}

/// Renders a modal's header bar: the title and a close button.
pub fn modal_header(title: &str) -> Markup {
    html! {
        div class="modal-header" {
            span class="modal-title" { (title) }
            button type="button" class="modal-close" onclick="this.closest('dialog').close()" {
                svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" {
                    path d="M4 4 L12 12" {}
                    path d="M12 4 L4 12" {}
                }
            }
        }
    }
}

pub fn dashboard_layout(federation_name: &str, version: &str, content: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html {
            head {
                (common_head("Picomint"))
            }
            body {
                div class="topbar" {
                    div style="display: flex; align-items: baseline; gap: 10px" {
                        span class="topbar-name" { (federation_name) }
                        span style="font-size: 13px; color: var(--ink-muted)" { "v" (version) }
                    }
                    button type="button" class="btn btn-primary"
                        onclick="document.getElementById('actions-modal').showModal()" {
                        "Actions"
                    }
                }
                div class="page" {
                    (content)
                }
            }
        }
    }
}
