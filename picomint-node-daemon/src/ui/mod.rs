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
//! patterns. What the bind does not keep out is the operator's own browser
//! acting on behalf of some other website, so [`run`] refuses requests that
//! only a browser under foreign control would send: see [`reject_foreign`].
//!
//! Styling is a single hand-rolled stylesheet (`assets/style.css`); modals
//! are native `<dialog>` elements opened and closed with one-line inline
//! handlers, so htmx is the only vendored JS.

pub mod assets;
pub mod dashboard;
pub mod dkg;
pub mod setup;

use std::net::{IpAddr, SocketAddr};

use axum::Router;
use axum::extract::Request;
use axum::http::header::HOST;
use axum::http::uri::Authority;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::{Next, from_fn};
use axum::response::{IntoResponse, Response};
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

    axum::serve(
        listener,
        router.layer(from_fn(reject_foreign)).into_make_service(),
    )
    .await
    .expect("Failed to serve UI");
}

/// Refuses the two requests a website can make the operator's browser send
/// to a loopback service without any credentials: a DNS-rebound page reaches
/// us with its own domain in `Host` and would read `/backup-config` as
/// same-origin, and a cross-site form post or fetch carries a
/// `Sec-Fetch-Site` other than `same-origin`. Neither `Host` nor
/// `Sec-Fetch-Site` can be set by page script, which is what makes them
/// trustworthy here.
async fn reject_foreign(request: Request, next: Next) -> Response {
    if !host_allowed(request.headers()) {
        return (
            StatusCode::FORBIDDEN,
            "Host must be localhost or an IP address",
        )
            .into_response();
    }

    if !fetch_site_allowed(request.method(), request.headers()) {
        return (StatusCode::FORBIDDEN, "Cross-site request refused").into_response();
    }

    next.run(request).await
}

/// A DNS name in `Host` can be rebound to loopback by whoever controls it;
/// `localhost` and IP literals cannot, and they are all the documented
/// deployments ever put in the address bar.
fn host_allowed(headers: &HeaderMap) -> bool {
    headers
        .get(HOST)
        .and_then(|host| host.to_str().ok())
        .and_then(|host| host.parse::<Authority>().ok())
        .is_some_and(|authority| {
            let host = authority.host().trim_matches(|c| c == '[' || c == ']');

            host == "localhost" || host.parse::<IpAddr>().is_ok()
        })
}

/// Reads are left alone so a link to the dashboard keeps working; writes must
/// come from the dashboard itself. An absent header is a non-browser client
/// such as curl, which a website cannot drive.
fn fetch_site_allowed(method: &Method, headers: &HeaderMap) -> bool {
    method == Method::GET
        || method == Method::HEAD
        || headers
            .get("sec-fetch-site")
            .and_then(|site| site.to_str().ok())
            .is_none_or(|site| site == "same-origin" || site == "none")
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

#[cfg(test)]
mod tests {
    use axum::http::{HeaderName, HeaderValue};

    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        pairs
            .iter()
            .map(|pair| {
                (
                    HeaderName::from_static(pair.0),
                    HeaderValue::from_str(pair.1).unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn host_accepts_loopback_names_and_ip_literals() {
        assert!(host_allowed(&headers(&[("host", "127.0.0.1:3000")])));
        assert!(host_allowed(&headers(&[("host", "localhost:3000")])));
        assert!(host_allowed(&headers(&[("host", "localhost")])));
        assert!(host_allowed(&headers(&[("host", "[::1]:3000")])));
        assert!(host_allowed(&headers(&[("host", "100.92.73.81:3000")])));
    }

    #[test]
    fn host_rejects_dns_names_and_absence() {
        assert!(!host_allowed(&headers(&[("host", "evil.example:3000")])));
        assert!(!host_allowed(&headers(&[(
            "host",
            "localhost.evil.example"
        )])));
        assert!(!host_allowed(&headers(&[("host", "")])));
        assert!(!host_allowed(&headers(&[])));
    }

    #[test]
    fn writes_need_same_origin_or_no_fetch_metadata() {
        assert!(fetch_site_allowed(
            &Method::POST,
            &headers(&[("sec-fetch-site", "same-origin")])
        ));
        assert!(fetch_site_allowed(
            &Method::POST,
            &headers(&[("sec-fetch-site", "none")])
        ));
        assert!(fetch_site_allowed(&Method::POST, &headers(&[])));
        assert!(!fetch_site_allowed(
            &Method::POST,
            &headers(&[("sec-fetch-site", "cross-site")])
        ));
        assert!(!fetch_site_allowed(
            &Method::POST,
            &headers(&[("sec-fetch-site", "same-site")])
        ));
    }

    #[test]
    fn reads_ignore_fetch_metadata() {
        assert!(fetch_site_allowed(
            &Method::GET,
            &headers(&[("sec-fetch-site", "cross-site")])
        ));
        assert!(fetch_site_allowed(
            &Method::HEAD,
            &headers(&[("sec-fetch-site", "cross-site")])
        ));
    }
}
