//! Node admin web UI.
//!
//! The UI runs in three phases on the same TCP port:
//!
//! - Setup UI (before the operator confirms the node set). Served by
//!   [`setup::router`] which takes an `Arc<SetupApi>` directly.
//! - DKG UI (after the operator clicks "Start DKG" until consensus comes
//!   up). Served by [`dkg::router`]: a stateless fallback router that
//!   returns the same waiting page for every path with status 503. The
//!   page polls `/` and redirects once the consensus UI starts answering
//!   with status 200.
//! - Dashboard UI (once the mint is running). Served by
//!   [`dashboard::router`] which takes an `Arc<ConsensusApi>` and
//!   dispatches to the freestanding module functions (`ecash`, `onchain`,
//!   `lightning`) over the shared `Server`.
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

/// Phosphor "regular" glyphs (MIT, phosphor-icons/core), the icon set
/// the Pico app draws from; each is the path data of the 256-unit icon.
pub mod phosphor {
    pub const TICKET: &str = "M232,104a8,8,0,0,0,8-8V64a16,16,0,0,0-16-16H32A16,16,0,0,0,16,64V96a8,8,0,0,0,8,8,24,24,0,0,1,0,48,8,8,0,0,0-8,8v32a16,16,0,0,0,16,16H224a16,16,0,0,0,16-16V160a8,8,0,0,0-8-8,24,24,0,0,1,0-48ZM32,167.2a40,40,0,0,0,0-78.4V64H88V192H32Zm192,0V192H104V64H224V88.8a40,40,0,0,0,0,78.4Z";
    pub const LIGHTNING: &str = "M215.79,118.17a8,8,0,0,0-5-5.66L153.18,90.9l14.66-73.33a8,8,0,0,0-13.69-7l-112,120a8,8,0,0,0,3,13l57.63,21.61L88.16,238.43a8,8,0,0,0,13.69,7l112-120A8,8,0,0,0,215.79,118.17ZM109.37,214l10.47-52.38a8,8,0,0,0-5-9.06L62,132.71l84.62-90.66L136.16,94.43a8,8,0,0,0,5,9.06l52.8,19.8Z";
    pub const DOWNLOAD_SIMPLE: &str = "M224,144v64a8,8,0,0,1-8,8H40a8,8,0,0,1-8-8V144a8,8,0,0,1,16,0v56H208V144a8,8,0,0,1,16,0Zm-101.66,5.66a8,8,0,0,0,11.32,0l40-40a8,8,0,0,0-11.32-11.32L136,124.69V32a8,8,0,0,0-16,0v92.69L93.66,98.34a8,8,0,0,0-11.32,11.32Z";
    pub const CALENDAR_BLANK: &str = "M208,32H184V24a8,8,0,0,0-16,0v8H88V24a8,8,0,0,0-16,0v8H48A16,16,0,0,0,32,48V208a16,16,0,0,0,16,16H208a16,16,0,0,0,16-16V48A16,16,0,0,0,208,32ZM72,48v8a8,8,0,0,0,16,0V48h80v8a8,8,0,0,0,16,0V48h24V80H48V48ZM208,208H48V96H208V208Z";
    pub const BROOM: &str = "M235.5,216.81c-22.56-11-35.5-34.58-35.5-64.8V134.73a15.94,15.94,0,0,0-10.09-14.87L165,110a8,8,0,0,1-4.48-10.34l21.32-53a28,28,0,0,0-16.1-37,28.14,28.14,0,0,0-35.82,16,.61.61,0,0,0,0,.12L108.9,79a8,8,0,0,1-10.37,4.49L73.11,73.14A15.89,15.89,0,0,0,55.74,76.8C34.68,98.45,24,123.75,24,152a111.45,111.45,0,0,0,31.18,77.53A8,8,0,0,0,61,232H232a8,8,0,0,0,3.5-15.19ZM67.14,88l25.41,10.3a24,24,0,0,0,31.23-13.45l21-53c2.56-6.11,9.47-9.27,15.43-7a12,12,0,0,1,6.88,15.92L145.69,93.76a24,24,0,0,0,13.43,31.14L184,134.73V152c0,.33,0,.66,0,1L55.77,101.71A108.84,108.84,0,0,1,67.14,88Zm48,128a87.53,87.53,0,0,1-24.34-42,8,8,0,0,0-15.49,4,105.16,105.16,0,0,0,18.36,38H64.44A95.54,95.54,0,0,1,40,152a85.9,85.9,0,0,1,7.73-36.29l137.8,55.12c3,18,10.56,33.48,21.89,45.16Z";
}

/// Renders one Phosphor glyph, sized by the stylesheet.
pub fn phosphor_icon(d: &str) -> Markup {
    html! {
        svg viewBox="0 0 256 256" fill="currentColor" aria-hidden="true" {
            path d=(d) {}
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

pub fn dashboard_layout(mint_name: &str, version: &str, content: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html {
            head {
                (common_head("Picomint"))
            }
            body {
                div class="topbar" {
                    div style="display: flex; align-items: baseline; gap: 10px" {
                        span class="topbar-name" { (mint_name) }
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
