//! Static asset routes for the admin UI (vendored htmx + style sheet).
//! Every file under `picomint-node-daemon/assets/` gets mounted under
//! `/assets/...` by [`WithStaticRoutesExt::with_static_routes`].

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::LazyLock;

use axum::Router;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

pub const HTMX_JS_ROUTE: &str = "/assets/htmx.org-2.0.4.min.js";
pub const STYLE_CSS_ROUTE: &str = "/assets/style.css";

pub const STYLE_CSS: &str = include_str!("../../assets/style.css");

/// Stylesheet href with a content-hash query param, so the aggressive
/// cache-control below can never serve a stale stylesheet after an upgrade.
pub static STYLE_CSS_HREF: LazyLock<String> = LazyLock::new(|| {
    let mut hasher = DefaultHasher::new();

    STYLE_CSS.hash(&mut hasher);

    format!("{STYLE_CSS_ROUTE}?v={:016x}", hasher.finish())
});

pub(crate) fn get_static_asset(content_type: &'static str, body: &'static [u8]) -> Response {
    (
        [(CONTENT_TYPE, content_type)],
        [(CACHE_CONTROL, format!("public, max-age={}", 60 * 60))],
        body,
    )
        .into_response()
}

pub(crate) fn get_static_css(body: &'static str) -> Response {
    get_static_asset("text/css", body.as_bytes())
}

pub(crate) fn get_static_js(body: &'static str) -> Response {
    get_static_asset("application/javascript", body.as_bytes())
}

pub trait WithStaticRoutesExt {
    fn with_static_routes(self) -> Self;
}

impl<S> WithStaticRoutesExt for Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    fn with_static_routes(self) -> Self {
        self.route(
            HTMX_JS_ROUTE,
            get(
                || async move { get_static_js(include_str!("../../assets/htmx.org-2.0.4.min.js")) },
            ),
        )
        .route(
            STYLE_CSS_ROUTE,
            get(|| async move { get_static_css(STYLE_CSS) }),
        )
    }
}
