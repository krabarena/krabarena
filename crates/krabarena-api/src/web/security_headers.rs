//! Baseline security-header middleware (since 0.26.0).
//!
//! Sets a conservative set of hardening headers on every response:
//!
//!   - `X-Content-Type-Options: nosniff` — stop MIME sniffing.
//!   - `X-Frame-Options: SAMEORIGIN` — clickjacking guard (legacy
//!     browsers; modern ones honour CSP `frame-ancestors`).
//!   - `Referrer-Policy: strict-origin-when-cross-origin` — don't leak
//!     full URLs (slugs, query) to third parties.
//!   - `Strict-Transport-Security` — force HTTPS for a year. Safe: the
//!     apex serves only over TLS and the `.org` host already 308s to
//!     `.com` (see `web::redirect`).
//!   - `Content-Security-Policy` — nonce-based `script-src`.
//!
//! ## Script policy
//!
//! `script-src` carries **no** `'unsafe-inline'`: an HTML injection or
//! a `javascript:` URL no longer executes. Every executable script is
//! either
//!
//!   - an external file from `'self'` (the Vite bundle, `/static/*.js`)
//!     or an allowlisted host (gtag.js from `www.googletagmanager.com`,
//!     injected by `frontend/src/analytics.js`; reCAPTCHA Enterprise
//!     from `www.google.com/recaptcha/` + `www.gstatic.com/recaptcha/`),
//!     or
//!   - an inline `<script nonce="…">` carrying the per-response nonce.
//!
//! The nonce is 128 random bits, freshly generated for every response
//! and exposed to the request's handler through the [`CSP_NONCE`]
//! task-local (read with [`current_nonce`]). `PageMeta::new` copies it
//! into `meta.csp_nonce`, so templates write
//! `<script nonce="{{ meta.csp_nonce }}">`; non-template HTML (the
//! limiter's 429 page) calls [`current_nonce`] directly. This layer is
//! the outermost in `build_router`, so the limiter, auth, redirect and
//! every handler run inside the task-local scope.
//!
//! Templates contain no inline event handlers (`onclick=` …) or
//! `javascript:` hrefs — enforced by `frontend/scripts/lint-templates.mjs`.
//! Behaviour lives in the Vite bundle (`addEventListener` + `data-*`).
//!
//! `'strict-dynamic'` is deliberately NOT used: the two third-party
//! loaders are host-allowlisted and inject only from allowlisted hosts.
//!
//! `style-src` still allows `'unsafe-inline'` (inline `style=` from
//! htmx's indicator styles, lottie, dynamic-styles.js) — out of scope
//! for the script hardening.
//!
//! When `VITE_DEV_SERVER_URL` is set (local `npm run dev`, never in
//! prod) its origin is added to `script-src` / `connect-src` so the
//! dev-server bundle and HMR socket load.
//!
//! `insert` (not `append`) so we never duplicate a header a handler
//! already set; these are set on the way out, so they are authoritative.

use std::sync::OnceLock;

use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use base64::Engine as _;
use rand::RngCore as _;

tokio::task_local! {
    /// Per-response CSP nonce, set by [`apply`] around the inner service.
    pub static CSP_NONCE: String;
}

/// The CSP nonce of the response currently being built, if this code is
/// running inside the [`apply`] middleware (it is, for every request
/// served by `build_router`). `None` in unit tests that render a
/// template directly or in work moved to a `tokio::spawn`ed task.
pub fn current_nonce() -> Option<String> {
    CSP_NONCE.try_with(|n| n.clone()).ok()
}

/// 128 bits from the OS CSPRNG, standard base64 (CSP `base64-value`).
pub fn generate_nonce() -> String {
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Static header pairs applied to every response.
const HEADERS: &[(&str, &str)] = &[
    ("x-content-type-options", "nosniff"),
    ("x-frame-options", "SAMEORIGIN"),
    ("referrer-policy", "strict-origin-when-cross-origin"),
    (
        "strict-transport-security",
        "max-age=31536000; includeSubDomains",
    ),
];

/// Origin of the local Vite dev server (`VITE_DEV_SERVER_URL`), cached.
/// Ignored when `NODE_ENV=production`.
fn vite_dev_origin() -> Option<&'static str> {
    static CACHED: OnceLock<Option<String>> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            if std::env::var("NODE_ENV").ok().as_deref() == Some("production") {
                return None;
            }
            std::env::var("VITE_DEV_SERVER_URL")
                .ok()
                .and_then(|u| dev_origin(&u))
        })
        .as_deref()
}

/// `http://127.0.0.1:5173/` → `http://127.0.0.1:5173`.
fn dev_origin(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = rest.split('/').next().filter(|h| !h.is_empty())?;
    // Refuse anything that could smuggle extra CSP tokens.
    if host
        .chars()
        .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-' | '[' | ']')))
    {
        return None;
    }
    Some(format!("{scheme}://{host}"))
}

/// Build the `Content-Security-Policy` value for one response.
pub fn content_security_policy(nonce: &str, dev_origin: Option<&str>) -> String {
    let (dev_script, dev_connect) = match dev_origin {
        Some(o) => {
            let ws = o.replacen("http", "ws", 1);
            (format!(" {o}"), format!(" {o} {ws}"))
        }
        None => (String::new(), String::new()),
    };
    // reCAPTCHA Enterprise (score-based) needs its script origins
    // whitelisted or the CSP blocks token minting and every
    // captcha-gated sign-in fails with CAPTCHA_FAILED:
    //   - script-src: www.google.com/recaptcha/ loads the enterprise.js
    //     loader, which injects the runtime from www.gstatic.com/recaptcha/.
    //   - frame-src:  execute() mints the token via a hidden anchor iframe
    //     served from www.google.com/recaptcha/.
    // Path-scoped so only the reCAPTCHA subtrees are allowed, not the whole
    // google.com/gstatic.com origins.
    format!(
        "default-src 'self'; \
         script-src 'self' 'nonce-{nonce}' https://www.googletagmanager.com https://www.google.com/recaptcha/ https://www.gstatic.com/recaptcha/{dev_script}; \
         script-src-attr 'none'; \
         style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; \
         font-src 'self' https://fonts.gstatic.com data:; \
         img-src 'self' data: https:; \
         connect-src 'self' https://www.google-analytics.com{dev_connect}; \
         frame-src 'self' https://www.google.com/recaptcha/; \
         object-src 'none'; \
         base-uri 'self'; \
         frame-ancestors 'self'"
    )
}

pub async fn apply(req: Request, next: Next) -> Response {
    let nonce = generate_nonce();
    let mut resp = CSP_NONCE.scope(nonce.clone(), next.run(req)).await;
    let h = resp.headers_mut();
    for (name, value) in HEADERS {
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            h.insert(n, v);
        }
    }
    if let Ok(v) = HeaderValue::from_str(&content_security_policy(&nonce, vite_dev_origin())) {
        h.insert(HeaderName::from_static("content-security-policy"), v);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::routing::get;
    use tower::util::ServiceExt as _;

    fn app() -> Router {
        Router::new()
            .route(
                "/",
                get(|| async { current_nonce().unwrap_or_else(|| "NO-NONCE".into()) }),
            )
            .layer(axum::middleware::from_fn(apply))
    }

    async fn fetch() -> (String, String) {
        let resp = app()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let csp = resp
            .headers()
            .get("content-security-policy")
            .expect("CSP header present")
            .to_str()
            .unwrap()
            .to_string();
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        (csp, String::from_utf8(body.to_vec()).unwrap())
    }

    fn directive<'a>(csp: &'a str, name: &str) -> &'a str {
        csp.split(';')
            .map(str::trim)
            .find(|d| d.split_whitespace().next() == Some(name))
            .unwrap_or_else(|| panic!("{name} directive missing in {csp}"))
    }

    fn nonce_of(script_src: &str) -> &str {
        script_src
            .split_whitespace()
            .find_map(|t| t.strip_prefix("'nonce-")?.strip_suffix('\''))
            .expect("script-src carries a nonce")
    }

    /// Regression guard: `'unsafe-inline'` in script-src let any HTML
    /// injection or `javascript:` URL execute (the 0.26.0 baseline policy).
    #[tokio::test]
    async fn script_src_has_nonce_and_no_unsafe_inline() {
        let (csp, body) = fetch().await;
        let script_src = directive(&csp, "script-src");
        assert!(
            !script_src.contains("'unsafe-inline'"),
            "script-src must not allow inline: {script_src}"
        );
        assert!(!script_src.contains("'unsafe-eval'"), "{script_src}");
        let nonce = nonce_of(script_src);
        assert!(nonce.len() >= 22, "nonce too short: {nonce}");
        // The handler sees the same nonce the header advertises.
        assert_eq!(body, nonce);
        assert_eq!(directive(&csp, "script-src-attr"), "script-src-attr 'none'");
        assert_eq!(directive(&csp, "object-src"), "object-src 'none'");
    }

    #[tokio::test]
    async fn nonce_differs_per_request() {
        let (a, _) = fetch().await;
        let (b, _) = fetch().await;
        assert_ne!(
            nonce_of(directive(&a, "script-src")),
            nonce_of(directive(&b, "script-src"))
        );
    }

    #[test]
    fn no_nonce_outside_middleware() {
        assert_eq!(current_nonce(), None);
    }

    #[test]
    fn dev_origin_parsing() {
        assert_eq!(
            dev_origin("http://127.0.0.1:5173/").as_deref(),
            Some("http://127.0.0.1:5173")
        );
        assert_eq!(dev_origin("javascript:alert(1)"), None);
        assert_eq!(dev_origin("http://evil; script-src *"), None);
        let csp = content_security_policy("abc", Some("http://127.0.0.1:5173"));
        assert!(directive(&csp, "script-src").ends_with("http://127.0.0.1:5173"));
        assert!(directive(&csp, "connect-src").contains("ws://127.0.0.1:5173"));
    }
}
